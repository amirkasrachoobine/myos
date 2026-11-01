package org.myos.bus;

import org.myos.bus.v1.Status;

/** A failed bus call. */
public class BusException extends Exception {
    public enum Kind {
        /** Couldn't reach the broker, or the connection broke. */
        IO,
        /** The bus or the service answered with an error; see {@link #code()}. */
        STATUS,
        /** No reply in time. */
        TIMEOUT,
        /** The peer broke the wire format. */
        PROTOCOL,
    }

    private final Kind kind;
    private final Status.Code code;

    public BusException(Kind kind, String message) {
        this(kind, message, Status.Code.OK, null);
    }

    public BusException(Kind kind, String message, Status.Code code, Throwable cause) {
        super(message, cause);
        this.kind = kind;
        this.code = code;
    }

    public Kind kind() {
        return kind;
    }

    /** For {@link Kind#STATUS}: why the call failed. {@code OK} otherwise. */
    public Status.Code code() {
        return code;
    }
}
