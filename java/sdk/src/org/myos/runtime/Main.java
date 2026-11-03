package org.myos.runtime;

import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.util.concurrent.TimeUnit;
import org.myos.sdk.App;
import org.myos.sdk.AppContext;

/**
 * What jrun starts: {@code java ... org.myos.runtime.Main [ARGS]}, with the
 * app described in myos.app.* system properties. Loads the app's main class
 * and runs it: {@link App#run} if it implements App, else its static
 * main(String[]).
 *
 * Its own messages go to stderr with syslog priorities (init passes them
 * to logd under the app's service name).
 */
public final class Main {
    private static final long STOP_GRACE_MS = 5000;

    public static void main(String[] args) {
        AppContext app;
        try {
            app = AppContext.get();
        } catch (IllegalStateException e) {
            System.err.println("<3>" + e.getMessage());
            System.exit(2);
            return;
        }
        org.myos.sdk.Lifecycle.setArgs(app, args);
        String mainClass = System.getProperty("myos.app.main", "");
        System.err.printf("<6>starting %s %s (%s), Java %s, pid %d, permissions: %s%n", app.id(), app.version(),
                mainClass, System.getProperty("java.version"), ProcessHandle.current().pid(),
                app.permissions().isEmpty() ? "none" : String.join(", ", app.permissions()));

        Thread mainThread = Thread.currentThread();
        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            org.myos.sdk.Lifecycle.beginStop(app);
            try {
                mainThread.join(STOP_GRACE_MS);
            } catch (InterruptedException ignored) {
            }
        }, "app-stop"));

        Class<?> cls;
        try {
            cls = Class.forName(mainClass, true, Main.class.getClassLoader());
        } catch (ClassNotFoundException | LinkageError e) {
            System.err.println("<3>can't load the app's main class " + mainClass + ": " + e);
            System.exit(2);
            return;
        }
        try {
            if (App.class.isAssignableFrom(cls)) {
                App a = (App) cls.getDeclaredConstructor().newInstance();
                a.run(app);
                if (!app.stopping()) System.err.println("<6>" + app.id() + " finished");
                exitSoon(0);
            } else {
                Method m = cls.getMethod("main", String[].class);
                if (!Modifier.isStatic(m.getModifiers())) throw new NoSuchMethodException("main isn't static");
                m.invoke(null, (Object) args);
                // An ordinary main: the JVM exits when its threads are done.
            }
        } catch (NoSuchMethodException e) {
            System.err.println("<3>" + mainClass + " neither implements org.myos.sdk.App nor has a static main(String[])");
            System.exit(2);
        } catch (InvocationTargetException e) {
            crash(app, e.getCause());
        } catch (Throwable t) {
            crash(app, t);
        }
    }

    private static void crash(AppContext app, Throwable t) {
        System.err.println("<3>" + app.id() + " crashed: " + t);
        for (StackTraceElement el : t.getStackTrace()) {
            System.err.println("<3>    at " + el);
        }
        exitSoon(1);
    }

    /** System.exit, but not from inside a shutdown (it would hang). */
    private static void exitSoon(int status) {
        Thread t = new Thread(() -> System.exit(status), "app-exit");
        t.setDaemon(false);
        t.start();
        try {
            t.join(TimeUnit.SECONDS.toMillis(1));
        } catch (InterruptedException ignored) {
        }
    }
}
