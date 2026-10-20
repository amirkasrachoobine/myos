//! Phase 0 smoke test: proves the Rust cross toolchain produces a working
//! binary for the target. Phase 1's init will live alongside this crate.

fn main() {
    let pid = std::process::id();
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| "unknown".into());
    println!(
        "hello from Rust on {}-{} (kernel {kernel}, pid {pid})",
        std::env::consts::ARCH,
        std::env::consts::OS
    );
}
