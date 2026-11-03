package org.myos.sdk;

/**
 * An app's entry point. The class named by {@code main} in app.toml either
 * implements this (and has a public no-argument constructor), or has an
 * ordinary {@code public static void main(String[])} and calls
 * {@link AppContext#get()} itself.
 *
 * <pre>{@code
 * public final class Hello implements App {
 *     public void run(AppContext app) throws Exception {
 *         app.log().info("hello from %s", app.id());
 *         while (app.sleep(Duration.ofSeconds(10))) {
 *             app.log().info("memory used: %d KiB", app.sysinfo().memory().getUsedKb());
 *         }
 *     }
 * }
 * }</pre>
 *
 * When run() returns the app exits with status 0; an exception exits with
 * status 1 after logging it. When the service is stopped, the JVM gets
 * SIGTERM: {@link AppContext#stopping()} turns true, {@link AppContext#sleep}
 * returns false, and the app has a few seconds to return from run().
 */
@FunctionalInterface
public interface App {
    void run(AppContext app) throws Exception;
}
