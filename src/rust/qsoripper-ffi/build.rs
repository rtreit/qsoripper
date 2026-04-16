//! Build script that generates the C header via cbindgen.

fn main() {
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|e| panic!("CARGO_MANIFEST_DIR not set: {e}"));

    let config = cbindgen::Config::from_file("cbindgen.toml")
        .unwrap_or_else(|e| panic!("Failed to read cbindgen.toml: {e}"));

    cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(config)
        .generate()
        .unwrap_or_else(|e| panic!("Unable to generate C bindings: {e}"))
        .write_to_file("qsoripper_ffi.h");
}
