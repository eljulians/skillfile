use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// Locate the `skillfile` binary in the workspace target directory.
///
/// `cargo-llvm-cov` (and potentially other tools) override the target
/// directory via `CARGO_TARGET_DIR`. The deprecated `Command::cargo_bin()`
/// and the `cargo_bin_cmd!` macro (which requires same-package) both fail
/// in that scenario. This function checks `CARGO_TARGET_DIR` first, then
/// falls back to the workspace-root `target/` directory.
fn skillfile_bin() -> PathBuf {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("target")
        });

    target_dir.join(profile).join("skillfile")
}

/// Build an `assert_cmd::Command` for the `skillfile` binary, rooted in `dir`.
pub fn sf(dir: &Path) -> Command {
    let mut cmd = Command::new(skillfile_bin());
    cmd.current_dir(dir);
    cmd
}

/// Build an `assert_cmd::Command` for the `skillfile` binary (no working dir).
pub fn skillfile_cmd() -> Command {
    Command::new(skillfile_bin())
}
