fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Running build.rs...");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set by Cargo");
    println!("OUT_DIR: {out_dir}");

    tonic_prost_build::configure()
        .build_server(true)
        .compile_protos(
            &[
                "proto/graph_tally.proto",
                "proto/uint128.proto",
                "proto/tap_aggregator_legacy.proto",
            ],
            &["proto"],
        )?;

    Ok(())
}
