use anyhow::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=proto");

    prost_build::Config::new()
        .out_dir("src/pb")
        .compile_protos(&["proto/x402.proto"], &["proto"])?;

    Ok(())
}
