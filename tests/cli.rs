//! Binary CLI contract tests.

#[test]
fn version_flag_prints_package_version() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_maul"))
        .arg("--version")
        .output()
        .expect("run maul --version");

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "version output was {stdout:?}"
    );
}
