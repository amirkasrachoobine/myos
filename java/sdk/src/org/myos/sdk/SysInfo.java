package org.myos.sdk;

import com.google.protobuf.Message;
import com.google.protobuf.Parser;
import org.myos.bus.BusClient;
import org.myos.bus.BusException;
import org.myos.sysinfo.v1.CpuReply;
import org.myos.sysinfo.v1.CpuRequest;
import org.myos.sysinfo.v1.DisksReply;
import org.myos.sysinfo.v1.DisksRequest;
import org.myos.sysinfo.v1.MemoryReply;
import org.myos.sysinfo.v1.MemoryRequest;
import org.myos.sysinfo.v1.ProcessesReply;
import org.myos.sysinfo.v1.ProcessesRequest;
import org.myos.sysinfo.v1.SystemReply;
import org.myos.sysinfo.v1.SystemRequest;

/**
 * System information from sysinfod: the same figures myos-sysinfo, ps and
 * top show (see proto/myos/sysinfo/v1/sysinfo.proto). Needs the "sysinfo"
 * permission.
 */
public final class SysInfo {
    private final AppContext app;
    private BusClient bus;

    SysInfo(AppContext app) {
        this.app = app;
    }

    public SystemReply system() {
        return call("System", SystemRequest.getDefaultInstance(), SystemReply.parser());
    }

    public CpuReply cpu() {
        return call("Cpu", CpuRequest.getDefaultInstance(), CpuReply.parser());
    }

    public MemoryReply memory() {
        return call("Memory", MemoryRequest.getDefaultInstance(), MemoryReply.parser());
    }

    public DisksReply disks() {
        return call("Disks", DisksRequest.getDefaultInstance(), DisksReply.parser());
    }

    public ProcessesReply processes() {
        return call("Processes", ProcessesRequest.getDefaultInstance(), ProcessesReply.parser());
    }

    private synchronized <R extends Message> R call(String method, Message req, Parser<R> parser) {
        app.require("sysinfo", "sysinfo." + method);
        try {
            if (bus == null) bus = app.connect();
            return bus.call("sysinfo", method, req, parser);
        } catch (BusException e) {
            if (bus != null) bus.close();
            bus = null;
            throw new SdkException("sysinfo." + method + ": " + e.getMessage(), e);
        }
    }
}
