//! §F5b — emit the on-disk path of the fixture cdylib so tests can load it.
//!
//! The fixture (`extensions-fixture-dylib`, `crate-type = ["cdylib","rlib"]`)
//! is a **dev-dependency** of this crate, so `cargo test -p
//! codesmith-extensions --lib` builds its cdylib into `<target>/<profile>/`.
//! `OUT_DIR` is `<target>/<profile>/build/<hash>/out`; popping three
//! components yields `<target>/<profile>`, where the cdylib lands. This avoids
//! shelling out to `cargo` from build.rs (no target-dir lock deadlock) — the
//! dev-dep mechanism builds the artifact; build.rs only computes the path (the
//! file exists by test-run time).

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set for build script");
    let mut target_profile = std::path::PathBuf::from(out_dir);
    for _ in 0..3 {
        target_profile.pop();
    }
    let libname = format!(
        "{}extensions_fixture_dylib.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    );
    let artifact = target_profile.join(libname);
    println!(
        "cargo:rustc-env=CODESMITH_FIXTURE_DYLIB={}",
        artifact.display()
    );
    println!("cargo:rerun-if-changed=../extensions-fixture-dylib/src/lib.rs");
}
