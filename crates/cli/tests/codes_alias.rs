//! The `codes` shorthand must behave exactly like `codesmith`: same output,
//! same exit status. A launcher that dropped arguments or swallowed the exit
//! code would still "work" interactively, so pin the forwarding contract.

use std::process::Command;

#[test]
fn codes_forwards_to_codesmith() {
    let alias = Command::new(env!("CARGO_BIN_EXE_codes"))
        .arg("--version")
        .output()
        .expect("run codes --version");
    let canonical = Command::new(env!("CARGO_BIN_EXE_codesmith"))
        .arg("--version")
        .output()
        .expect("run codesmith --version");

    assert_eq!(alias.status.code(), canonical.status.code());
    assert_eq!(alias.stdout, canonical.stdout);
}
