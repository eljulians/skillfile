use std::path::{Path, PathBuf};

use assert_cmd::Command;

fn skillfile_bin_from_test_dir() -> Option<PathBuf> {
    find_skillfile_bin_above(&std::env::current_exe().ok()?)
}

/// Walk up from a test executable to the nearest `skillfile` binary.
///
/// Cargo has placed test executables at `<profile>/deps/` and, with the newer
/// build-dir layout, at `<profile>/build/<pkg>/<hash>/out/`, while the binary
/// stays at `<profile>/skillfile`. Walking up covers both. The `is_file` check
/// matters: `<profile>/build/skillfile/` is the CLI package's build directory.
fn find_skillfile_bin_above(test_exe: &Path) -> Option<PathBuf> {
    let bin_name = format!("skillfile{}", std::env::consts::EXE_SUFFIX);
    test_exe
        .ancestors()
        .skip(1)
        .map(|dir| dir.join(&bin_name))
        .find(|candidate| candidate.is_file())
}

/// Locate the `skillfile` binary.
///
/// Strategy:
/// 1. Derive the target directory from this test binary's own path (see
///    [`find_skillfile_bin_above`]), so the binary comes from the same target
///    directory as the test. This is what makes `cargo llvm-cov` exercise its
///    instrumented binary instead of an uninstrumented `target/debug` one.
///
/// 2. If the binary doesn't exist there (e.g. `cargo llvm-cov` only builds
///    test targets via `--tests`, not the binary executable), fall back to
///    `CARGO_TARGET_DIR` or the workspace-root `target/` directory. CI must
///    pre-build the binary (`cargo build -p skillfile`) so this fallback
///    finds a fresh binary, not a stale cached one.
pub fn skillfile_bin() -> PathBuf {
    // Try same target dir as the running test binary.
    if let Some(bin) = skillfile_bin_from_test_dir() {
        return bin;
    }

    // Fallback: CARGO_TARGET_DIR or workspace target/.
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    let target_dir = std::env::var("CARGO_TARGET_DIR").map_or_else(
        |_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("target")
        },
        PathBuf::from,
    );

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

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    fn bin_name() -> String {
        format!("skillfile{}", std::env::consts::EXE_SUFFIX)
    }

    #[test]
    fn finds_binary_for_test_in_deps_dir() {
        let target = tempfile::tempdir().unwrap();
        let profile = target.path().join("debug");
        touch(&profile.join(bin_name()));
        let test_exe = profile.join("deps/cli-0123abcd");
        touch(&test_exe);

        assert_eq!(
            find_skillfile_bin_above(&test_exe),
            Some(profile.join(bin_name()))
        );
    }

    #[test]
    fn finds_binary_for_test_in_build_dir_layout() {
        let target = tempfile::tempdir().unwrap();
        let profile = target.path().join("debug");
        touch(&profile.join(bin_name()));
        // The CLI package's own build directory shares the binary's name.
        std::fs::create_dir_all(profile.join("build").join(bin_name()).join("f00d/out")).unwrap();
        let test_exe = profile.join("build/skillfile-functional-tests/0123abcd/out/cli-0123abcd");
        touch(&test_exe);

        assert_eq!(
            find_skillfile_bin_above(&test_exe),
            Some(profile.join(bin_name()))
        );
    }

    #[test]
    fn no_binary_when_cli_was_not_built() {
        let target = tempfile::tempdir().unwrap();
        let test_exe = target.path().join("debug/deps/cli-0123abcd");
        touch(&test_exe);

        assert_eq!(find_skillfile_bin_above(&test_exe), None);
    }
}
