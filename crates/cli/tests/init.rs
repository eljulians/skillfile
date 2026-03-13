/// Integration tests for `skillfile init` command.
///
/// Run with: cargo test --test init
use skillfile::commands::init::cmd_init;

#[test]
fn init_fails_without_tty() {
    // In test context, stdin is not a TTY, so cmd_init should fail.
    let dir = tempfile::tempdir().unwrap();
    let result = cmd_init(dir.path());
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("interactive terminal"), "got: {msg}");
}
