use shinki_emu_build::{write_core_build_manifest, BuildAsset, CoreBuildManifest};
use std::{env, path::PathBuf};

fn main() {
    for name in ["HOST", "TARGET", "PROFILE"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    let mut manifest = CoreBuildManifest::new("ethornell-rs", "rust-cdylib")
        .expect("Cargo must provide the core build target and profile");
    assert!(
        matches!(manifest.target.as_str(), "x86_64-pc-windows-msvc" | "x86_64-unknown-linux-gnu"),
        "ethornell-rs embedded adapter requires Windows x64/MSVC or Linux x64/GNU"
    );
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("locating the fork source root");
    manifest.licenses = vec![
        BuildAsset { source: root.join("LICENSE-MPL-2.0"), dest: "licenses/ethornell-rs/LICENSE-MPL-2.0".into() },
        BuildAsset { source: root.join("MODIFICATIONS.md"), dest: "licenses/ethornell-rs/MODIFICATIONS.md".into() },
    ];
    write_core_build_manifest(&manifest).expect("writing ethornell-rs core-build.json");
}
