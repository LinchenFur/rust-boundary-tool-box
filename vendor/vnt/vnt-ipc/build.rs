fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().unwrap();
    // SAFETY: Build scripts run single-threaded for this crate before prost-build
    // looks up protoc, so setting the process environment is scoped to codegen.
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    let mut config = prost_build::Config::new();
    config.protoc_arg("--experimental_allow_proto3_optional");
    config
        .compile_protos(&["proto/local_ipc.proto"], &["proto"])
        .unwrap();
}
