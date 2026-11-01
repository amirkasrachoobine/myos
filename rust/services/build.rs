//! Generates the Rust types for proto/myos/{log,net,sysinfo}/v1/*.proto.
//! Same rules as rust/bus/build.rs: the protos are found through
//! $MYOS_PROTO_DIR (set by Buildroot) or at ../../proto, protoc through
//! $PROTOC or PATH.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=MYOS_PROTO_DIR");
    println!("cargo:rerun-if-env-changed=PROTOC");

    let dir = match std::env::var_os("MYOS_PROTO_DIR") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proto"),
    };
    let files: Vec<PathBuf> = ["log/v1/log.proto", "net/v1/net.proto", "sysinfo/v1/sysinfo.proto"]
        .iter()
        .map(|f| dir.join("myos").join(f))
        .collect();
    for f in &files {
        if !f.exists() {
            panic!("{} not found; set MYOS_PROTO_DIR to the repo's proto/ directory", f.display());
        }
        println!("cargo:rerun-if-changed={}", f.display());
    }
    prost_build::Config::new()
        .compile_protos(&files, &[&dir])
        .unwrap_or_else(|e| panic!("protoc failed: {e}\n(is protoc installed, or PROTOC set?)"));
}
