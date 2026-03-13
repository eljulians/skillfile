/// Integration tests for `skillfile init` command.
///
/// Run with: cargo test --test init
use assert_cmd::cargo_bin_cmd;
use predicates::prelude::*;

#[test]
fn init_fails_without_tty() {
    // assert_cmd pipes stdin, so is_terminal() returns false.
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = cargo_bin_cmd!("skillfile");
    cmd.current_dir(dir.path());
    cmd.arg("init");
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("interactive terminal"));
}
