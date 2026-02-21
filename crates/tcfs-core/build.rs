fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use vendored protoc binary — no system installation required
    let protoc_path = protoc_bin_vendored::protoc_bin_path()
        .expect("protoc-bin-vendored: no binary for this platform");
    std::env::set_var("PROTOC", protoc_path);

    // tonic 0.14: use tonic_prost_build (compile_protos → compile)
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(&["src/proto/tcfs.proto"], &["src/proto"])?;

    println!("cargo:rerun-if-changed=src/proto/tcfs.proto");
    Ok(())
}
