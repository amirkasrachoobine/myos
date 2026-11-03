package org.myos.sdk;

import org.myos.bus.BusException;

/** A system service call failed (the service is down, or it said no). */
public class SdkException extends RuntimeException {
    public SdkException(String message, Throwable cause) {
        super(message, cause);
    }

    /** The bus failure behind this, if there was one. */
    public BusException busException() {
        return getCause() instanceof BusException b ? b : null;
    }
}
