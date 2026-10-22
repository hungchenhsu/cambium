use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("../../vendor/enroute/proto");
    let files = [
        "enroute/common/v1alpha1/common.proto",
        "enroute/api/v1alpha1/repository.proto",
        "enroute/api/v1alpha1/ref.proto",
        "enroute/api/v1alpha1/object.proto",
        "enroute/api/v1alpha1/commit.proto",
        "enroute/api/v1alpha1/sync.proto",
        "enroute/hook/v1alpha1/hook.proto",
    ];
    println!("cargo:rerun-if-changed={}", root.display());
    let descriptors = protox::compile(files.map(|file| root.join(file)), [&root])?;
    tonic_prost_build::configure()
        .build_server(false)
        .compile_fds(descriptors)?;
    Ok(())
}
