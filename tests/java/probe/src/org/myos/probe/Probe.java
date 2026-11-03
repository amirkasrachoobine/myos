package org.myos.probe;

import com.google.protobuf.StringValue;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.NetworkInterface;
import java.net.Socket;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import org.myos.bus.BusClient;
import org.myos.bus.BusException;
import org.myos.bus.BusService;
import org.myos.log.v1.WriteReply;
import org.myos.log.v1.WriteRequest;
import org.myos.net.v1.ListInterfacesReply;
import org.myos.net.v1.ListInterfacesRequest;
import org.myos.sdk.App;
import org.myos.sdk.AppContext;
import org.myos.sdk.PermissionException;
import org.myos.sysinfo.v1.MemoryReply;
import org.myos.sysinfo.v1.MemoryRequest;

/**
 * Test app for tests/java_test.py: tries what an app with the permissions
 * ["log", "call:app.org.myos.hello"] may and may not do, and prints one
 * "RESULT name value" line per check. Goes around the SDK's own checks
 * (raw bus calls, plain java.nio), so what it reports is what the system
 * enforces.
 */
public final class Probe implements App {
    private static void result(String name, Object value) {
        System.out.println("RESULT " + name + " " + value);
    }

    private static String call(BusClient bus, String service, String method, com.google.protobuf.Message req) {
        try {
            bus.callRaw(service, method, req.toByteString());
            return "ok";
        } catch (BusException e) {
            return e.kind() == BusException.Kind.STATUS ? e.code().name() : e.kind().name();
        }
    }

    private static String register(String name) {
        try (BusService s = BusService.register(name)) {
            return "ok";
        } catch (BusException e) {
            return e.kind() == BusException.Kind.STATUS ? e.code().name() : e.kind().name();
        }
    }

    private static String write(Path p) {
        try {
            Files.writeString(p, "probe\n");
            return "ok";
        } catch (IOException e) {
            return e.getClass().getSimpleName();
        }
    }

    private static String read(Path p) {
        try {
            Files.readAllBytes(p);
            return "ok";
        } catch (IOException e) {
            return e.getClass().getSimpleName();
        }
    }

    @Override
    public void run(AppContext app) throws Exception {
        for (String line : Files.readAllLines(Path.of("/proc/self/status"))) {
            if (line.startsWith("Uid:")) result("uid", line.split("\\s+")[1]);
            if (line.startsWith("Gid:")) result("gid", line.split("\\s+")[1]);
            if (line.startsWith("Groups:")) result("groups", line.substring(7).trim().isEmpty() ? "none" : line.substring(7).trim());
            if (line.startsWith("NoNewPrivs:")) result("no_new_privs", line.split("\\s+")[1]);
        }
        result("cwd", System.getProperty("user.dir"));
        result("args", String.join(",", app.args()));

        try (BusClient bus = BusClient.connect()) {
            result("bus_ping", call(bus, "bus", "Ping", org.myos.bus.v1.PingRequest.newBuilder().setMessage("probe").build()));
            result("log_write", call(bus, "log", "Write", WriteRequest.newBuilder().setTag("app.org.myos.probe").setMessage("probe was here").build()));
            result("sysinfo_memory", call(bus, "sysinfo", "Memory", MemoryRequest.getDefaultInstance()));
            result("net_list", call(bus, "net", "ListInterfaces", ListInterfacesRequest.getDefaultInstance()));
            result("hello_status", call(bus, "app.org.myos.hello", "Status", StringValue.of("")));
        }
        try {
            app.sysinfo().memory();
            result("sdk_sysinfo", "ok");
        } catch (PermissionException e) {
            result("sdk_sysinfo", "PermissionException");
        }
        result("log_sdk_seq", app.log().write(6, "probe via the SDK") > 0 ? "ok" : "fallback");

        result("register_own", register("app.org.myos.probe"));
        result("register_own_sub", register("app.org.myos.probe.extra"));
        result("register_other_app", register("app.org.myos.hello"));
        result("register_system", register("log"));

        List<String> ifaces = new ArrayList<>();
        for (NetworkInterface n : java.util.Collections.list(NetworkInterface.getNetworkInterfaces())) {
            ifaces.add(n.getName() + (n.isUp() ? "+" : "-"));
        }
        ifaces.sort(null);
        result("interfaces", String.join(",", ifaces));
        try (Socket s = new Socket()) {
            s.connect(new InetSocketAddress("10.0.2.2", 9), 2000);
            result("connect_outside", "ok");
        } catch (IOException e) {
            result("connect_outside", e.getClass().getSimpleName());
        }
        try {
            app.net().connect("10.0.2.2", 9, java.time.Duration.ofSeconds(1)).close();
            result("sdk_connect", "ok");
        } catch (PermissionException e) {
            result("sdk_connect", "PermissionException");
        }
        // Loopback works inside its own namespace.
        try (java.net.ServerSocket ss = new java.net.ServerSocket(0, 1, java.net.InetAddress.getLoopbackAddress());
             Socket c = new Socket(java.net.InetAddress.getLoopbackAddress(), ss.getLocalPort())) {
            result("loopback", "ok");
        } catch (IOException e) {
            result("loopback", e.getClass().getSimpleName());
        }

        result("write_own_data", app.files().path("probe.txt") != null ? write(app.files().path("probe.txt")) : "?");
        try {
            app.files().path("../../org.myos.hello/data/runs");
            result("sdk_escape", "ok");
        } catch (IllegalArgumentException e) {
            result("sdk_escape", "IllegalArgumentException");
        }
        result("read_other_app_data", read(Path.of("/var/lib/myos/apps/org.myos.hello/data/runs")));
        result("write_etc", write(Path.of("/etc/probe-was-here")));
        result("write_own_package", write(app.home().resolve("app.toml")));
        result("write_policy", write(Path.of("/etc/myos/bus/policy.d/" + System.getProperty("probe.uid", "20001"))));
        result("write_tmp", write(Path.of("/tmp/probe-tmp-file")));
        result("tmp_listing", String.join(",", new java.io.File("/tmp").list()));
        result("done", "yes");
    }
}
