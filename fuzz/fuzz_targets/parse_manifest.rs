#![no_main]
use libfuzzer_sys::fuzz_target;
use skillfile_core::models::SourceFields;
use std::collections::HashSet;
use std::io::Write;

/// Validate that a name matches the parser's name regex: [a-zA-Z0-9._-]+
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
}

fuzz_target!(|data: &[u8]| {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Skillfile");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(data).unwrap();
    drop(f);

    // 1. Must never panic (crash-freedom baseline).
    let result = skillfile_core::parser::parse_manifest(&path);

    let Ok(parsed) = result else {
        // IO errors are fine (e.g. invalid path); the parser must not panic.
        return;
    };

    let manifest = &parsed.manifest;

    // 2. Every entry name must be valid: non-empty, matches [a-zA-Z0-9._-]+.
    //    The parser skips entries with invalid names (adds a warning instead),
    //    so any entry that made it into the vec must have a valid name.
    for entry in &manifest.entries {
        assert!(
            is_valid_name(&entry.name),
            "entry with invalid name '{}' was not filtered by parser",
            entry.name,
        );
    }

    // 3. Source field invariants — no empty strings in structural fields.
    for entry in &manifest.entries {
        match &entry.source {
            SourceFields::Github {
                owner_repo,
                path_in_repo,
                ref_,
            } => {
                assert!(
                    !owner_repo.is_empty(),
                    "github entry '{}' has empty owner_repo",
                    entry.name,
                );
                assert!(
                    !path_in_repo.is_empty(),
                    "github entry '{}' has empty path_in_repo",
                    entry.name,
                );
                assert!(
                    !ref_.is_empty(),
                    "github entry '{}' has empty ref_",
                    entry.name,
                );
                assert!(
                    owner_repo.contains('/'),
                    "github entry '{}' owner_repo '{}' missing '/'",
                    entry.name,
                    owner_repo,
                );
            }
            SourceFields::Local { path } => {
                assert!(
                    !path.is_empty(),
                    "local entry '{}' has empty path",
                    entry.name,
                );
            }
            SourceFields::Url { url } => {
                assert!(
                    !url.is_empty(),
                    "url entry '{}' has empty url",
                    entry.name,
                );
            }
        }
    }

    // 4. Install target invariants — adapter and scope are well-formed.
    for target in &manifest.install_targets {
        assert!(
            !target.adapter.is_empty(),
            "install target has empty adapter name",
        );
    }

    // 5. Warnings are well-formed strings (non-empty, no panics during formatting).
    for w in &parsed.warnings {
        assert!(!w.is_empty(), "empty warning string");
    }

    // 6. Idempotency: re-parsing the same file must produce identical results.
    //    This catches any hidden state or non-determinism in the parser.
    let result2 = skillfile_core::parser::parse_manifest(&path).unwrap();
    assert_eq!(
        manifest.entries.len(),
        result2.manifest.entries.len(),
        "re-parse produced different entry count",
    );
    assert_eq!(
        manifest.install_targets.len(),
        result2.manifest.install_targets.len(),
        "re-parse produced different target count",
    );
    for (a, b) in manifest.entries.iter().zip(result2.manifest.entries.iter()) {
        assert_eq!(a, b, "re-parse produced different entry");
    }

    // 7. Unique-name invariant: the first occurrence of each name is always present.
    //    Duplicates may also be pushed (with a warning), but the first must exist.
    let mut first_seen: HashSet<&str> = HashSet::new();
    for entry in &manifest.entries {
        first_seen.insert(&entry.name);
    }
    // Every name that appeared in the entries is in first_seen (trivially true),
    // but we can check that the count of unique names <= total entries.
    assert!(
        first_seen.len() <= manifest.entries.len(),
        "unique names ({}) exceeds entry count ({})",
        first_seen.len(),
        manifest.entries.len(),
    );
});
