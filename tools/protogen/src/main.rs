//! Regenerates `src/proto/anolis.deviceprovider.v1.rs` from the vendored
//! `proto/` tree. Run it with `just gen`; CI fails when the committed file is
//! out of date. protox compiles the schema in Rust, so no `protoc` is needed.

use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut protos: Vec<_> = std::fs::read_dir(root.join("proto/anolis/deviceprovider/v1"))
        .expect("read proto/")
        .map(|entry| entry.expect("read proto entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "proto"))
        .collect();
    protos.sort();
    let descriptors = protox::compile(&protos, [root.join("proto")]).expect("compile the protocol schema");
    prost_build::Config::new()
        .out_dir(root.join("src/proto"))
        .compile_fds(descriptors)
        .expect("generate the protocol bindings");
}
