package org.myos.bus;

import com.google.protobuf.ByteString;
import com.google.protobuf.InvalidProtocolBufferException;
import com.google.protobuf.Message;
import com.google.protobuf.Parser;
import java.io.IOException;
import java.net.StandardProtocolFamily;
import java.net.UnixDomainSocketAddress;
import java.nio.ByteBuffer;
import java.nio.channels.SelectionKey;
import java.nio.channels.Selector;
import java.nio.channels.SocketChannel;
import java.time.Duration;
import java.util.List;
import org.myos.bus.v1.Call;
import org.myos.bus.v1.Frame;
import org.myos.bus.v1.ListServicesReply;
import org.myos.bus.v1.ListServicesRequest;
import org.myos.bus.v1.PingReply;
import org.myos.bus.v1.PingRequest;
import org.myos.bus.v1.ServiceInfo;
import org.myos.bus.v1.Status;

/**
 * Java client for the myos IPC bus.
 *
 * <pre>{@code
 * try (BusClient bus = BusClient.connect()) {
 *     PingReply pong = bus.ping("hello");
 *     // Any service's method, with its generated request/reply classes:
 *     MemoryReply mem = bus.call("sysinfo", "Memory", MemoryRequest.getDefaultInstance(), MemoryReply.parser());
 * }
 * }</pre>
 *
 * One call at a time; open several clients for concurrent calls. The wire
 * format is described in proto/myos/bus/v1/wire.proto.
 */
public final class BusClient implements AutoCloseable {
    public static final String DEFAULT_SOCKET = "/run/myos/bus.sock";
    public static final int MAX_FRAME = 16 << 20;
    /** The broker is told the same deadline and normally answers first. */
    private static final long GRACE_MS = 1000;

    private SocketChannel channel;
    private final Selector selector;
    private long nextId = 0;
    private Duration timeout = Duration.ofSeconds(5);

    /** $MYOS_BUS_SOCKET, or /run/myos/bus.sock. */
    public static String socketPath() {
        String env = System.getenv("MYOS_BUS_SOCKET");
        return env != null && !env.isEmpty() ? env : DEFAULT_SOCKET;
    }

    public static BusClient connect() throws BusException {
        return connect(socketPath());
    }

    public static BusClient connect(String path) throws BusException {
        SocketChannel ch = null;
        try {
            ch = SocketChannel.open(StandardProtocolFamily.UNIX);
            ch.connect(UnixDomainSocketAddress.of(path));
            ch.configureBlocking(false);
            Selector sel = Selector.open();
            return new BusClient(ch, sel);
        } catch (IOException e) {
            if (ch != null) {
                try {
                    ch.close();
                } catch (IOException ignored) {
                }
            }
            throw new BusException(BusException.Kind.IO, "can't connect to " + path + ": " + e.getMessage(),
                    Status.Code.OK, e);
        }
    }

    private BusClient(SocketChannel channel, Selector selector) {
        this.channel = channel;
        this.selector = selector;
    }

    /** Per-call timeout; default 5 s. */
    public void setTimeout(Duration timeout) {
        this.timeout = timeout.isNegative() || timeout.isZero() ? Duration.ofMillis(1) : timeout;
    }

    @Override
    public void close() {
        try {
            if (channel != null) channel.close();
            selector.close();
        } catch (IOException ignored) {
        }
        channel = null;
    }

    /** Calls service.method with a request message; parses the reply with {@code parser}. */
    public <R extends Message> R call(String service, String method, Message request, Parser<R> parser)
            throws BusException {
        ByteString bytes = callRaw(service, method, request.toByteString());
        try {
            return parser.parseFrom(bytes);
        } catch (InvalidProtocolBufferException e) {
            throw new BusException(BusException.Kind.PROTOCOL, service + "." + method + " reply didn't decode",
                    Status.Code.OK, e);
        }
    }

    /** Calls service.method with a serialized payload; returns the serialized reply. */
    public synchronized ByteString callRaw(String service, String method, ByteString payload) throws BusException {
        if (channel == null) {
            throw new BusException(BusException.Kind.IO, "connection closed after an earlier error");
        }
        long id = ++nextId;
        Frame request = Frame.newBuilder()
                .setId(id)
                .setCall(Call.newBuilder()
                        .setService(service)
                        .setMethod(method)
                        .setPayload(payload)
                        .setTimeoutMs((int) Math.min(timeout.toMillis(), 0xFFFFFFFFL)))
                .build();
        Frame in;
        try {
            long deadline = System.nanoTime() + (timeout.toMillis() + GRACE_MS) * 1_000_000L;
            writeFrame(request, deadline);
            do {
                in = readFrame(deadline);
            } while (in.getId() != id); // anything else isn't ours; can't normally happen
            if (!in.hasReply()) {
                throw new BusException(BusException.Kind.PROTOCOL, "expected a reply");
            }
        } catch (BusException e) {
            close(); // mid-frame or a late reply could follow: don't reuse
            throw e;
        }
        Status st = in.getReply().getStatus();
        if (st.getCode() != Status.Code.OK) {
            String what = st.getCode().name() + (st.getMessage().isEmpty() ? "" : ": " + st.getMessage());
            throw new BusException(BusException.Kind.STATUS, what, st.getCode(), null);
        }
        return in.getReply().getPayload();
    }

    /** Round trip through the broker. */
    public PingReply ping(String message) throws BusException {
        return call("bus", "Ping", PingRequest.newBuilder().setMessage(message).build(), PingReply.parser());
    }

    /** Services currently on the bus. */
    public List<ServiceInfo> listServices() throws BusException {
        return call("bus", "ListServices", ListServicesRequest.getDefaultInstance(), ListServicesReply.parser())
                .getServicesList();
    }

    private void writeFrame(Frame frame, long deadline) throws BusException {
        byte[] body = frame.toByteArray();
        if (body.length > MAX_FRAME) throw new BusException(BusException.Kind.PROTOCOL, "request too large");
        ByteBuffer buf = ByteBuffer.allocate(4 + body.length); // big-endian by default
        buf.putInt(body.length).put(body).flip();
        try {
            while (buf.hasRemaining()) {
                if (channel.write(buf) == 0) await(SelectionKey.OP_WRITE, deadline);
            }
        } catch (IOException e) {
            throw new BusException(BusException.Kind.IO, "bus connection: " + e.getMessage(), Status.Code.OK, e);
        }
    }

    private Frame readFrame(long deadline) throws BusException {
        ByteBuffer hdr = readExact(4, deadline);
        int n = hdr.getInt();
        if (n < 0 || n > MAX_FRAME) throw new BusException(BusException.Kind.PROTOCOL, "oversized frame");
        try {
            return Frame.parseFrom(readExact(n, deadline));
        } catch (InvalidProtocolBufferException e) {
            throw new BusException(BusException.Kind.PROTOCOL, "undecodable frame", Status.Code.OK, e);
        }
    }

    private ByteBuffer readExact(int n, long deadline) throws BusException {
        ByteBuffer buf = ByteBuffer.allocate(n);
        try {
            while (buf.hasRemaining()) {
                int r = channel.read(buf);
                if (r < 0) throw new BusException(BusException.Kind.IO, "the broker closed the connection");
                if (r == 0) await(SelectionKey.OP_READ, deadline);
            }
        } catch (IOException e) {
            throw new BusException(BusException.Kind.IO, "bus connection: " + e.getMessage(), Status.Code.OK, e);
        }
        return buf.flip();
    }

    private void await(int op, long deadline) throws IOException, BusException {
        long leftMs = (deadline - System.nanoTime()) / 1_000_000L;
        if (leftMs <= 0) throw new BusException(BusException.Kind.TIMEOUT, "timed out waiting for a reply");
        SelectionKey key = channel.register(selector, op);
        try {
            selector.select(leftMs);
            selector.selectedKeys().clear();
        } finally {
            key.interestOps(0);
        }
    }
}
