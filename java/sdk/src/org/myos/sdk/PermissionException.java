package org.myos.sdk;

/**
 * The app used something its manifest doesn't ask for. The SDK throws this
 * before trying, for a clear message; the system refuses it anyway (busd
 * for bus services, the kernel for the network and other apps' files).
 */
public class PermissionException extends SecurityException {
    private final String permission;

    public PermissionException(String permission, String what) {
        super(what + " needs the \"" + permission + "\" permission; add it to permissions in app.toml");
        this.permission = permission;
    }

    public String permission() {
        return permission;
    }
}
