//! Runs `grbresearch selftest`: the tool attaches read-only to its own process only.

use std::path::PathBuf;
use std::process::Command;

fn test_dir() -> PathBuf {
    let base = std::env::var_os("AURORA_TEST_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("grbresearch-tests");
    base.join(format!("selftest-it-{}", std::process::id()))
}

#[test]
fn selftest_against_own_process() {
    let dir = test_dir();
    let out = Command::new(env!("CARGO_BIN_EXE_grbresearch"))
        .args(["selftest", "--dir"])
        .arg(&dir)
        .output()
        .expect("run grbresearch selftest");
    let text = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "selftest failed:\n{text}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("selftest ok"), "{text}");
    assert!(!text.contains("FAIL"), "{text}");
}

#[test]
fn help_says_read_only() {
    let out = Command::new(env!("CARGO_BIN_EXE_grbresearch")).arg("--help").output().expect("run --help");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("PROCESS_QUERY_INFORMATION | PROCESS_VM_READ"));
    assert!(text.contains("never writes memory"));
}
