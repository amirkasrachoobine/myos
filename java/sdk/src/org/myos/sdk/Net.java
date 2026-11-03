package org.myos.sdk;

import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.http.HttpClient;
import java.time.Duration;
import java.util.List;
import org.myos.bus.BusClient;
import org.myos.bus.BusException;
import org.myos.net.v1.Interface;
import org.myos.net.v1.ListInterfacesReply;
import org.myos.net.v1.ListInterfacesRequest;

/**
 * The network.
 *
 * Connections need the "network" permission. An app without it runs in a
 * network namespace of its own that has only a loopback interface, so it
 * can talk to itself on 127.0.0.1 but reach nothing else; the checks here
 * only make the error clearer.
 *
 * Interface status (what netd reports, as in myos-net) needs "netinfo".
 */
public final class Net {
    private final AppContext app;
    private BusClient bus;

    Net(AppContext app) {
        this.app = app;
    }

    /** Whether this app may open network connections. */
    public boolean available() {
        return app.has("network");
    }

    /** A TCP connection, with a connect timeout. */
    public Socket connect(String host, int port, Duration timeout) throws IOException {
        app.require("network", "connecting to " + host + ":" + port);
        Socket s = new Socket();
        try {
            s.connect(new InetSocketAddress(host, port), (int) Math.min(Integer.MAX_VALUE, timeout.toMillis()));
        } catch (IOException e) {
            s.close();
            throw e;
        }
        return s;
    }

    /** java.net.http's client, for HTTP and HTTPS. */
    public HttpClient http() {
        app.require("network", "HTTP");
        return HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(10)).build();
    }

    /** The system's interfaces, addresses, gateway and DNS servers, from netd. */
    public synchronized List<Interface> interfaces() {
        app.require("netinfo", "net.ListInterfaces");
        try {
            if (bus == null) bus = app.connect();
            return bus.call("net", "ListInterfaces", ListInterfacesRequest.getDefaultInstance(),
                    ListInterfacesReply.parser()).getInterfacesList();
        } catch (BusException e) {
            if (bus != null) bus.close();
            bus = null;
            throw new SdkException("net.ListInterfaces: " + e.getMessage(), e);
        }
    }
}
