package org.myos.sdk;

/** For org.myos.runtime.Main only. */
public final class Lifecycle {
    private Lifecycle() {}

    public static void setArgs(AppContext app, String[] args) {
        app.setArgs(args);
    }

    public static void beginStop(AppContext app) {
        app.beginStop();
    }
}
