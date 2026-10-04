//! §F5b — emit the on-disk path of the fixture cdylib so tests can load it.
//!
//! The fixture (`extensions-fixture-dylib`, `crate-type = ["cdylib","rlib"]`)
//! is a **dev-dependency** of this crate, so `cargo test -p
//! codesmith-extensions --lib` builds its cdylib into the build graph.
//! `OUT_DIR` is `<target>/<profile>/build/<hash>/out`; popping three
//! components yields `<target>/<profile>`, where the cdylib would land. This
//! avoids shelling out to `cargo` from build.rs (no target-dir lock deadlock).
//!
//! Caveat: the un-hashed export link at that path only exists when the fixture
//! is built as a direct target; when it is built purely as a dev-dependency
//! (e.g. `cargo test --workspace` on a fresh environment) the artifact stays
//! in `<target>/<profile>/deps/` under a content-hashed name. Tests must not
//! rely on the emitted path alone — resolve via
//! [`crate::test_support::fixture_dylib_path`], which falls back to the
//! `deps/` artifact.

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
