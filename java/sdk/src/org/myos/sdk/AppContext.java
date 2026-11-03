package org.myos.sdk;

import java.nio.file.Path;
import java.time.Duration;
import java.util.Arrays;
import java.util.Collections;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;
import java.util.concurrent.CopyOnWriteArrayList;
import org.myos.bus.BusClient;
import org.myos.bus.BusException;
import org.myos.bus.BusService;

/**
 * Everything an app gets from the system: who it is, its files, the log,
 * system information, the network and the bus. There is one per JVM; jrun
 * describes the app to it through system properties (myos.app.*).
 */
public final class AppContext {
    private static volatile AppContext instance;

    private final String id, name, version;
    private final Path home, data;
    private final Set<String> permissions;
    private final Log log;
    private final Files files;
    private final SysInfo sysinfo;
    private final Net net;
    private final List<Runnable> stopHooks = new CopyOnWriteArrayList<>();
    private final Object stopLock = new Object();
    private volatile boolean stopping;
    private volatile List<String> args = List.of();

    private AppContext(String id, String name, String version, Path home, Path data, Set<String> permissions) {
        this.id = id;
        this.name = name;
        this.version = version;
        this.home = home;
        this.data = data;
        this.permissions = Collections.unmodifiableSet(permissions);
        this.log = new Log(this);
        this.files = new Files(data);
        this.sysinfo = new SysInfo(this);
        this.net = new Net(this);
    }

    /** The running app's context. Throws if the JVM wasn't started by jrun. */
    public static AppContext get() {
        AppContext c = instance;
        if (c != null) return c;
        synchronized (AppContext.class) {
            if (instance == null) {
                String id = System.getProperty("myos.app.id");
                if (id == null || id.isEmpty()) {
                    throw new IllegalStateException("not running as a myos app (start it with `jrun run ID`)");
                }
                String perms = System.getProperty("myos.app.permissions", "");
                Set<String> set = new LinkedHashSet<>();
                for (String p : perms.split(",")) {
                    if (!p.isBlank()) set.add(p.trim());
                }
                instance = new AppContext(id, System.getProperty("myos.app.name", id),
                        System.getProperty("myos.app.version", "0"),
                        Path.of(System.getProperty("myos.app.home", ".")),
                        Path.of(System.getProperty("myos.app.data", System.getProperty("user.dir"))), set);
            }
            return instance;
        }
    }

    /** The app id from app.toml, e.g. "org.myos.hello". */
    public String id() {
        return id;
    }

    public String name() {
        return name;
    }

    public String version() {
        return version;
    }

    /** The bus name the app may serve: "app." + id. */
    public String serviceName() {
        return "app." + id;
    }

    /** The installed package (read-only): app.toml and the jars. */
    public Path home() {
        return home;
    }

    /** The app's arguments: `args` from app.toml, then any given to `jrun run ID ...`. */
    public List<String> args() {
        return args;
    }

    void setArgs(String[] a) {
        args = List.of(a);
    }

    /** The permissions granted in app.toml. */
    public Set<String> permissions() {
        return permissions;
    }

    public boolean has(String permission) {
        return permissions.contains(permission);
    }

    /** Throws {@link PermissionException} unless the app has {@code permission}. */
    public void require(String permission, String what) {
        if (!has(permission)) throw new PermissionException(permission, what);
    }

    public Log log() {
        return log;
    }

    public Files files() {
        return files;
    }

    public SysInfo sysinfo() {
        return sysinfo;
    }

    public Net net() {
        return net;
    }

    /**
     * A bus connection of the app's own, for services the SDK doesn't wrap
     * (other apps: the "call:SERVICE" permission). busd decides what it may
     * call; close it when done.
     */
    public BusClient connect() throws BusException {
        return BusClient.connect();
    }

    /**
     * Serves {@link #serviceName()} on the bus from a background thread, and
     * registers again if busd restarts. Returns at once.
     */
    public void serve(BusService.Handler handler) {
        Thread t = new Thread(() -> {
            boolean complained = false;
            while (!stopping) {
                try (BusService svc = BusService.register(serviceName())) {
                    onStop(svc::close);
                    if (complained) log.notice("serving %s on the bus again", serviceName());
                    complained = false;
                    svc.serve(handler);
                } catch (BusException e) {
                    if (!complained && !stopping) {
                        System.err.println("<4>can't serve " + serviceName() + " on the bus: " + e.getMessage() + "; retrying");
                        complained = true;
                    }
                }
                sleep(Duration.ofSeconds(1));
            }
        }, "app-service");
        t.setDaemon(true);
        t.start();
    }

    /** True once the app has been asked to stop (SIGTERM, myctl stop). */
    public boolean stopping() {
        return stopping;
    }

    /**
     * Sleeps, but wakes up early when the app is asked to stop. Returns
     * false if it's stopping, so {@code while (app.sleep(d)) { ... }} is a
     * service's main loop.
     */
    public boolean sleep(Duration d) {
        long end = System.nanoTime() + d.toNanos();
        synchronized (stopLock) {
            while (!stopping) {
                long left = end - System.nanoTime();
                if (left <= 0) return true;
                try {
                    stopLock.wait(Math.max(1, left / 1_000_000L));
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                    return !stopping;
                }
            }
        }
        return false;
    }

    /** Runs {@code hook} when the app is asked to stop, before the JVM exits. */
    public void onStop(Runnable hook) {
        stopHooks.add(hook);
    }

    /** Called by the runtime on SIGTERM. */
    void beginStop() {
        synchronized (stopLock) {
            if (stopping) return;
            stopping = true;
            stopLock.notifyAll();
        }
        for (Runnable r : stopHooks) {
            try {
                r.run();
            } catch (RuntimeException e) {
                System.err.println("<4>stop hook failed: " + e);
            }
        }
    }

    @Override
    public String toString() {
        return id + " " + version + " " + Arrays.toString(permissions.toArray());
    }
}
