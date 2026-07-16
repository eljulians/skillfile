#![cfg(unix)]
//! Interactive TUI and wizard tests: spawn the real binary in a PTY
//! and drive interactive flows with expect-style scripting.
//!
//! These tests verify that the full keypress-to-output pipeline works:
//! terminal setup/teardown, TUI event loop, cliclack/inquire prompts.
//!
//! Unix-only: rexpect requires a PTY, which is not available on Windows.
//!
//! Run with: cargo test -p skillfile-functional-tests --test interactive

use std::process::Command;
use std::time::Duration;

use skillfile_functional_tests::skillfile_bin;

/// Default timeout for expect operations (15 seconds — generous for CI).
const TIMEOUT_MS: u64 = 15_000;

/// Build a Command for `skillfile init` with CI removed and a fake token.
fn init_cmd(dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(skillfile_bin());
    cmd.arg("init")
        .current_dir(dir)
        .env_remove("CI")
        .env("GITHUB_TOKEN", "ghp_fake_for_test");
    cmd
}

fn status_cmd(dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(skillfile_bin());
    cmd.arg("status")
        .current_dir(dir)
        .env("TERM", "xterm-256color")
        .env("CLICOLOR_FORCE", "1");
    cmd
}

// ---------------------------------------------------------------------------
// PTY sanity check
// ---------------------------------------------------------------------------

/// Verify rexpect can send input that a child process reads and outputs.
#[test]
fn pty_input_sanity_check() {
    let mut session =
        rexpect::session::spawn_command(Command::new("cat"), Some(5_000)).expect("spawn cat");
    session.send_line("hello_pty").expect("send input to cat");
    session
        .exp_string("hello_pty")
        .expect("cat should echo input back via stdout");
    session.send_control('d').expect("send EOF");
    session.exp_eof().ok();
}

#[test]
fn add_wizard_rejects_piped_stdin() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Skillfile"), "# empty\n").unwrap();

    // Keep stderr attached to rexpect's PTY while piping only stdin.
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "printf '\\n' | \"$SKILLFILE_BIN\" add"])
        .current_dir(dir.path())
        .env("SKILLFILE_BIN", skillfile_bin());

    let mut session = rexpect::session::spawn_command(cmd, Some(2_000))
        .expect("failed to spawn skillfile add with piped stdin");
    session
        .exp_string("interactive wizard requires a terminal")
        .expect("piped stdin should be rejected before the prompt starts");
    session
        .exp_eof()
        .expect("skillfile add should exit after rejecting piped stdin");
}

// ---------------------------------------------------------------------------
// init wizard
// ---------------------------------------------------------------------------

/// The cliclack multiselect prompt in `skillfile init` renders all 8 platforms.
/// Verify the prompt renders, then cancel cleanly with Ctrl+C.
#[test]
fn init_wizard_renders_platform_prompt() {
    let dir = tempfile::tempdir().unwrap();

    let mut session = rexpect::session::spawn_command(init_cmd(dir.path()), Some(TIMEOUT_MS))
        .expect("failed to spawn skillfile init in PTY");

    // cliclack renders the platform selection prompt.
    session
        .exp_string("Select platforms")
        .expect("should show platform selection prompt");

    // Cancel the wizard cleanly via signal.
    session.send_control('c').expect("send Ctrl+C");
    session.exp_eof().ok();

    // Cancelled init should not populate Skillfile.
    let sf_exists = dir.path().join("Skillfile").exists();
    let sf_empty = sf_exists
        && std::fs::read_to_string(dir.path().join("Skillfile"))
            .unwrap_or_default()
            .trim()
            .is_empty();
    assert!(
        !sf_exists || sf_empty,
        "cancelled init should not populate Skillfile"
    );
}

/// Drive the full init wizard through all prompts.
///
/// Currently ignored: `console` 0.15's `read_single_key()` uses
/// `tcsetattr(TCSADRAIN)` which blocks in rexpect's PTY environment,
/// preventing regular keystrokes (Space, Enter) from reaching cliclack's
/// input loop. Ctrl+C works because it's a signal (SIGINT), not a read().
///
/// The signal delivery test above confirms the PTY is functional.
/// Revisit when `console` or `rexpect` address this incompatibility.
#[test]
#[ignore = "console TCSADRAIN blocks in rexpect PTY — keystrokes don't reach cliclack"]
fn init_wizard_golden_path() {
    let dir = tempfile::tempdir().unwrap();

    let mut session = rexpect::session::spawn_command(init_cmd(dir.path()), Some(TIMEOUT_MS))
        .expect("failed to spawn init in PTY");

    // Wait for multiselect to fully render.
    session.exp_string("windsurf").expect("full platform list");
    std::thread::sleep(Duration::from_secs(2));

    // Toggle first platform + confirm.
    session.send(" ").expect("toggle platform");
    std::thread::sleep(Duration::from_secs(1));
    session.send("\r").expect("confirm platforms");

    // Scope selection.
    session.exp_string("scope").expect("scope prompt");
    std::thread::sleep(Duration::from_secs(1));
    session.send("\r").expect("confirm scope");

    // Destination.
    session
        .exp_string("config be stored")
        .expect("destination prompt");
    std::thread::sleep(Duration::from_secs(1));
    session.send("\r").expect("confirm destination");

    // Token.
    session.exp_string("token").expect("token step");
    session.exp_eof().expect("wizard should exit cleanly");

    assert!(
        dir.path().join("Skillfile").exists(),
        "init should create Skillfile"
    );
}

// ---------------------------------------------------------------------------
// search TUI (smoke test — render only)
// ---------------------------------------------------------------------------

/// Verify the search TUI initializes crossterm and enters alternate screen.
///
/// Uses a debug-only hidden harness command so the test covers terminal
/// lifecycle only, not live registry latency.
///
/// Does NOT test Esc/cancel: rexpect's PTY cannot deliver keystrokes to
/// crossterm's `event::read()` (same `tcsetattr(TCSADRAIN)` issue as
/// cliclack). We verify the TUI starts, then kill the process.
#[test]
fn search_tui_enters_alternate_screen() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(skillfile_bin());
    cmd.arg("__search-tui-test")
        .current_dir(dir.path())
        .env_remove("CI");
    let mut session = rexpect::session::spawn_command(cmd, Some(TIMEOUT_MS))
        .expect("failed to spawn search in PTY");

    // Alternate screen escape proves crossterm init + TUI render started.
    session
        .exp_string("\x1b[?1049h")
        .expect("TUI should enter alternate screen");

    // Kill the process (keystrokes don't reach crossterm through rexpect PTY).
    session.send_control('c').expect("send SIGINT");
    session.exp_eof().ok();
}

#[test]
fn status_renders_ansi_in_pty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    std::fs::write(dir.path().join("skills/foo.md"), "# Foo\n").unwrap();
    std::fs::write(
        dir.path().join("Skillfile"),
        "install  claude-code  local\n\
         local  skill  foo  skills/foo.md\n",
    )
    .unwrap();

    let mut session = rexpect::session::spawn_command(status_cmd(dir.path()), Some(TIMEOUT_MS))
        .expect("failed to spawn status in PTY");
    session
        .exp_string("\x1b[")
        .expect("status should render ANSI escapes on a TTY");
    session.exp_eof().expect("status should exit cleanly");
}
