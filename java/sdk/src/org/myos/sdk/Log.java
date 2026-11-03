package org.myos.sdk;

import java.util.List;
import org.myos.bus.BusClient;
import org.myos.bus.BusException;
import org.myos.log.v1.Entry;
import org.myos.log.v1.QueryReply;
import org.myos.log.v1.QueryRequest;
import org.myos.log.v1.WriteReply;
import org.myos.log.v1.WriteRequest;

/**
 * The system log (logd), with the app's service name ({@code app.<id>}) as
 * the tag, so {@code myctl logs app.<id>} shows these entries together with
 * the app's stdout and stderr. Needs the "log" permission.
 *
 * If logd can't be reached, entries go to stderr with a syslog priority
 * prefix instead, which init passes on to logd once it's back: nothing is
 * lost and nothing throws.
 */
public final class Log {
    public static final int EMERG = 0, ALERT = 1, CRIT = 2, ERR = 3, WARNING = 4, NOTICE = 5, INFO = 6, DEBUG = 7;

    private final AppContext app;
    private final String tag;
    private BusClient bus;

    Log(AppContext app) {
        this.app = app;
        this.tag = "app." + app.id();
    }

    public String tag() {
        return tag;
    }

    public void debug(String format, Object... args) {
        write(DEBUG, fmt(format, args));
    }

    public void info(String format, Object... args) {
        write(INFO, fmt(format, args));
    }

    public void notice(String format, Object... args) {
        write(NOTICE, fmt(format, args));
    }

    public void warning(String format, Object... args) {
        write(WARNING, fmt(format, args));
    }

    public void error(String format, Object... args) {
        write(ERR, fmt(format, args));
    }

    private static String fmt(String format, Object... args) {
        return args.length == 0 ? format : String.format(format, args);
    }

    /**
     * Writes one entry at a syslog priority (0 emerg ... 7 debug). Returns
     * the entry's sequence number in logd, or 0 if it went to stderr.
     */
    public synchronized long write(int priority, String message) {
        app.require("log", "writing to the system log");
        if (priority < 0 || priority > 7) throw new IllegalArgumentException("priority must be 0-7");
        try {
            if (bus == null) bus = app.connect();
            WriteReply r = bus.call("log", "Write",
                    WriteRequest.newBuilder().setTag(tag).setPriority(priority).setMessage(message).build(),
                    WriteReply.parser());
            return r.getSeq();
        } catch (BusException e) {
            if (bus != null) bus.close();
            bus = null;
            for (String line : message.split("\n", -1)) {
                System.err.println("<" + priority + ">" + line);
            }
            return 0;
        }
    }

    /**
     * The app's own entries, newest last: what it wrote through this class
     * (logd shows apps only entries written with their own uid).
     */
    public synchronized List<Entry> recent(int limit) {
        app.require("log", "reading the system log");
        try {
            if (bus == null) bus = app.connect();
            QueryReply r = bus.call("log", "Query",
                    QueryRequest.newBuilder().setTag(tag).setLimit(limit).build(), QueryReply.parser());
            return r.getEntriesList();
        } catch (BusException e) {
            if (bus != null) bus.close();
            bus = null;
            throw new SdkException("log.Query: " + e.getMessage(), e);
        }
    }
}
