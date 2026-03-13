use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// Locate the `skillfile` binary.
///
/// Strategy:
/// 1. Derive the target directory from this test binary's own path.
///    Cargo places test binaries in `<target>/<profile>/deps/`, so the
///    skillfile binary should be at `<target>/<profile>/skillfile`.
///    This is the correct path for `cargo test` and `cargo test --target-dir`.
///
/// 2. If the binary doesn't exist there (e.g. `cargo llvm-cov` only builds
///    test targets via `--tests`, not the binary executable), fall back to
///    `CARGO_TARGET_DIR` or the workspace-root `target/` directory. CI must
///    pre-build the binary (`cargo build -p skillfile`) so this fallback
///    finds a fresh binary, not a stale cached one.
fn skillfile_bin() -> PathBuf {
    // Try same target dir as the running test binary.
    if let Ok(test_exe) = std::env::current_exe() {
        if let Some(profile_dir) = test_exe.parent().and_then(|p| p.parent()) {
            let candidate = profile_dir.join("skillfile");
            if candidate.exists() {
                return candidate;
            }
        }
    }

    // Fallback: CARGO_TARGET_DIR or workspace target/.
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
