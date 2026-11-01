//! Generates the Rust types for proto/myos/bus/v1/*.proto with prost.
//!
//! The .proto files live at the top of the repo (shared with C++, Java and
//! Python). Buildroot copies only this crate's directory into its build
//! tree, so it passes the real location in MYOS_PROTO_DIR; a normal
//! `cargo build` in the repo finds them at ../../proto.
//! protoc comes from $PROTOC if set, else from PATH.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=MYOS_PROTO_DIR");
    println!("cargo:rerun-if-env-changed=PROTOC");

    let dir = match std::env::var_os("MYOS_PROTO_DIR") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proto"),
    };
    let files: Vec<PathBuf> = ["wire.proto", "bus.proto"]
        .iter()
        .map(|f| dir.join("myos/bus/v1").join(f))
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
