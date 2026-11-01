package org.myos.bus;

import org.myos.bus.v1.PingReply;

/**
 * myos-bus-ping-java [MESSAGE]: the Java client's Phase 2 check.
 * Exits 0 and prints {@code pong: "<message>" from busd X in N ms} on success.
 */
public final class Ping {
    public static void main(String[] args) {
        String message = args.length > 0 ? String.join(" ", args) : "hello from Java";
        try (BusClient bus = BusClient.connect()) {
            long t0 = System.nanoTime();
            PingReply pong = bus.ping(message);
            double ms = (System.nanoTime() - t0) / 1e6;
            System.out.printf("pong: \"%s\" from %s in %.2f ms%n",
                    pong.getMessage().replace("\\", "\\\\").replace("\"", "\\\""), pong.getBrokerVersion(), ms);
        } catch (BusException e) {
            System.err.println("myos-bus-ping-java: " + e.getMessage());
            System.exit(1);
        }
    }
}
