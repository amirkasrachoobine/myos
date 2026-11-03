package org.myos.bus;

import com.google.protobuf.ByteString;
import com.google.protobuf.InvalidProtocolBufferException;
import java.io.IOException;
import java.net.StandardProtocolFamily;
import java.net.UnixDomainSocketAddress;
import java.nio.ByteBuffer;
import java.nio.channels.SocketChannel;
import java.util.List;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import org.myos.bus.v1.Call;
import org.myos.bus.v1.Caller;
import org.myos.bus.v1.Frame;
import org.myos.bus.v1.Register;
import org.myos.bus.v1.Reply;
import org.myos.bus.v1.Status;

/**
 * Serves one or more names on the bus (Phase 6: apps serve {@code app.<id>}).
 *
 * <pre>{@code
 * try (BusService svc = BusService.register("app.org.example.notes")) {
 *     svc.serve(call -> switch (call.method()) {
 *         case "Count" -> CountReply.newBuilder().setN(n).build().toByteString();
 *         default -> throw BusService.Failure.unknownMethod(call.method());
 *     });
 * }
 * }</pre>
 *
 * Calls are handled on a small thread pool, so a slow call doesn't hold up
 * the others; replies may go out in any order, as the wire format allows.
 */
public final class BusService implements AutoCloseable {
    /** One incoming call. */
    public record Request(String service, String method, ByteString payload, Caller caller) {}

    @FunctionalInterface
    public interface Handler {
        /** Returns the serialized reply message, or throws {@link Failure}. */
        ByteString handle(Request request) throws Exception;
    }

    /** Thrown by a handler to answer with a particular status. */
    public static class Failure extends Exception {
        private final Status.Code code;

        public Failure(Status.Code code, String message) {
            super(message);
            this.code = code;
        }

        public Status.Code code() {
            return code;
        }

        public static Failure unknownMethod(String method) {
            return new Failure(Status.Code.UNKNOWN_METHOD, "no method \"" + method + "\"");
        }

        public static Failure badRequest(String why) {
            return new Failure(Status.Code.BAD_REQUEST, why);
        }
    }

    private final SocketChannel channel;
    private final Object writeLock = new Object();
    private volatile boolean closed;

    private BusService(SocketChannel channel) {
        this.channel = channel;
    }

    public static BusService register(String... names) throws BusException {
        return register(BusClient.socketPath(), List.of(names));
    }

    public static BusService register(String socket, List<String> names) throws BusException {
        SocketChannel ch;
        try {
            ch = SocketChannel.open(StandardProtocolFamily.UNIX);
            ch.connect(UnixDomainSocketAddress.of(socket));
        } catch (IOException e) {
            throw new BusException(BusException.Kind.IO, "can't connect to " + socket + ": " + e.getMessage(),
                    Status.Code.OK, e);
        }
        BusService svc = new BusService(ch);
        try {
            long id = 0;
            for (String name : names) {
                svc.write(Frame.newBuilder().setId(++id).setRegister(Register.newBuilder().setService(name)).build());
                Frame in = svc.read();
                if (in == null || !in.hasReply()) {
                    throw new BusException(BusException.Kind.PROTOCOL, "no reply to registering " + name);
                }
                Status st = in.getReply().getStatus();
                if (st.getCode() != Status.Code.OK) {
                    throw new BusException(BusException.Kind.STATUS, "can't register " + name + ": "
                            + st.getCode().name() + (st.getMessage().isEmpty() ? "" : ": " + st.getMessage()),
                            st.getCode(), null);
                }
            }
        } catch (IOException e) {
            svc.close();
            throw new BusException(BusException.Kind.IO, "bus connection: " + e.getMessage(), Status.Code.OK, e);
        } catch (BusException e) {
            svc.close();
            throw e;
        }
        return svc;
    }

    /**
     * Handles calls until the connection closes (the broker went away, or
     * {@link #close()}). Returns normally after close(); throws BusException
     * if the broker dropped the connection.
     */
    public void serve(Handler handler) throws BusException {
        ExecutorService pool = Executors.newFixedThreadPool(4, r -> {
            Thread t = new Thread(r, "bus-service");
            t.setDaemon(true);
            return t;
        });
        try {
            while (true) {
                Frame in;
                try {
                    in = read();
                } catch (IOException e) {
                    if (closed) return;
                    throw new BusException(BusException.Kind.IO, "bus connection: " + e.getMessage(), Status.Code.OK, e);
                }
                if (in == null) {
                    if (closed) return;
                    throw new BusException(BusException.Kind.IO, "the broker closed the connection");
                }
                if (!in.hasCall()) continue;
                long id = in.getId();
                Call c = in.getCall();
                pool.execute(() -> answer(id, handler, new Request(c.getService(), c.getMethod(), c.getPayload(), c.getCaller())));
            }
        } finally {
            pool.shutdownNow();
        }
    }

    private void answer(long id, Handler handler, Request req) {
        Reply.Builder reply = Reply.newBuilder();
        try {
            ByteString out = handler.handle(req);
            reply.setStatus(Status.newBuilder().setCode(Status.Code.OK)).setPayload(out == null ? ByteString.EMPTY : out);
        } catch (Failure f) {
            reply.setStatus(Status.newBuilder().setCode(f.code()).setMessage(String.valueOf(f.getMessage())));
        } catch (InvalidProtocolBufferException e) {
            reply.setStatus(Status.newBuilder().setCode(Status.Code.BAD_REQUEST).setMessage("request didn't decode"));
        } catch (Exception e) {
            reply.setStatus(Status.newBuilder().setCode(Status.Code.INTERNAL).setMessage(e.toString()));
        }
        try {
            write(Frame.newBuilder().setId(id).setReply(reply).build());
        } catch (IOException ignored) {
            // the reader sees the broken connection
        }
    }

    @Override
    public void close() {
        closed = true;
        try {
            channel.close();
        } catch (IOException ignored) {
        }
    }

    private void write(Frame frame) throws IOException {
        byte[] body = frame.toByteArray();
        ByteBuffer buf = ByteBuffer.allocate(4 + body.length);
        buf.putInt(body.length).put(body).flip();
        synchronized (writeLock) {
            while (buf.hasRemaining()) channel.write(buf);
        }
    }

    /** null at end of stream. */
    private Frame read() throws IOException {
        ByteBuffer hdr = readExact(4);
        if (hdr == null) return null;
        int n = hdr.getInt();
        if (n < 0 || n > BusClient.MAX_FRAME) throw new IOException("oversized frame");
        ByteBuffer body = readExact(n);
        if (body == null) return null;
        return Frame.parseFrom(body);
    }

    private ByteBuffer readExact(int n) throws IOException {
        ByteBuffer buf = ByteBuffer.allocate(n);
        while (buf.hasRemaining()) {
            if (channel.read(buf) < 0) return null;
        }
        return buf.flip();
    }
}
