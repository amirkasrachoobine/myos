package org.myos.hello;

import com.google.protobuf.StringValue;
import java.time.Duration;
import java.util.List;
import org.myos.bus.BusService;
import org.myos.sdk.App;
import org.myos.sdk.AppContext;
import org.myos.sdk.SdkException;
import org.myos.sysinfo.v1.MemoryReply;
import org.myos.sysinfo.v1.SystemReply;

/**
 * The sample app: counts its runs in its data directory, logs a heartbeat
 * with the system's load and memory every --interval seconds, and answers
 * "Status" on the bus as app.org.myos.hello (a google.protobuf.StringValue).
 */
public final class Hello implements App {
    private volatile String status = "starting";
    private volatile long beats;

    @Override
    public void run(AppContext app) throws Exception {
        int interval = 30;
        List<String> args = app.args();
        for (int i = 0; i < args.size() - 1; i++) {
            if (args.get(i).equals("--interval")) interval = Integer.parseInt(args.get(i + 1));
        }

        int runs = app.files().read("runs").map(s -> Integer.parseInt(s.trim())).orElse(0) + 1;
        app.files().write("runs", runs + "\n");

        SystemReply sys = app.sysinfo().system();
        app.log().notice("hello from Java: %s %s started (run %d) on %s, kernel %s, %d CPUs", app.id(), app.version(),
                runs, sys.getHostname(), sys.getKernelRelease(), sys.getCpuCount());
        System.out.println("stdout works too: init passes this line on to logd");
        app.log().info("network: %s", app.net().available() ? "yes" : "loopback only (no \"network\" permission)");

        app.serve(req -> switch (req.method()) {
            case "Status" -> StringValue.of(status + "; heartbeats " + beats + "; asked by uid "
                    + req.caller().getUid()).toByteString();
            default -> throw BusService.Failure.unknownMethod(req.method());
        });

        heartbeat(app);
        while (app.sleep(Duration.ofSeconds(interval))) {
            heartbeat(app);
        }
        app.log().notice("stopping after %d heartbeats", beats);
    }

    private void heartbeat(AppContext app) throws Exception {
        try {
            SystemReply s = app.sysinfo().system();
            MemoryReply m = app.sysinfo().memory();
            status = String.format("load %.2f, memory %d of %d MiB used, %d processes, up %ds", s.getLoad1(),
                    m.getUsedKb() / 1024, m.getTotalKb() / 1024, s.getProcesses(), (long) s.getUptimeSeconds());
        } catch (SdkException e) {
            status = "sysinfo unavailable: " + e.getMessage();
        }
        beats++;
        app.log().info("heartbeat %d: %s", beats, status);
        app.files().write("status", status + "\n");
    }
}
