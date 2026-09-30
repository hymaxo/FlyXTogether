use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=FLYX_XPLANE_SDK");

    let sdk = env::var_os("FLYX_XPLANE_SDK")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
                .join("../../third_party/xplane-sdk")
        });
    let header = sdk.join("CHeaders/XPLM/XPLMDataAccess.h");
    assert!(
        header.is_file(),
        "X-Plane SDK headers not found at {} (set FLYX_XPLANE_SDK)",
        header.display()
    );

    match env::var("CARGO_CFG_TARGET_OS").unwrap().as_str() {
        "windows" => {
            let lib_dir = sdk.join("Libraries/Win");
            assert!(
                lib_dir.join("XPLM_64.lib").is_file(),
                "XPLM_64.lib not found in {}",
                lib_dir.display()
            );
            println!("cargo:rustc-link-search=native={}", lib_dir.display());
            println!("cargo:rustc-link-lib=dylib=XPLM_64");
        }
        "macos" => {
            let fw_dir = sdk.join("Libraries/Mac");
            assert!(
                fw_dir.join("XPLM.framework").exists(),
                "XPLM.framework not found in {} (run scripts/fetch-xplane-sdk.sh)",
                fw_dir.display()
            );
            println!("cargo:rustc-link-search=framework={}", fw_dir.display());
            println!("cargo:rustc-link-lib=framework=XPLM");
        }
        // Linux: XPLM symbols are resolved by X-Plane at load time.
        _ => {}
    }
}
