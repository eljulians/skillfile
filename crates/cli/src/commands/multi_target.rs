use std::collections::BTreeMap;
use std::path::PathBuf;

use skillfile_core::error::SkillfileError;

use crate::commands::installed_variants::{DirVariant, SingleFileVariant};

pub(crate) type DirContentMap = BTreeMap<String, String>;

pub(crate) struct SingleFileDiff<'a> {
    pub(crate) entry_name: &'a str,
    pub(crate) sha: &'a str,
    pub(crate) target_label: &'a str,
    pub(crate) upstream: &'a str,
    pub(crate) installed_text: &'a str,
}

pub(crate) fn divergent_targets_message(entry_name: &str, labels: &[String]) -> SkillfileError {
    SkillfileError::Manifest(format!(
        "'{entry_name}' has divergent edits across install targets: {} — reconcile them before pinning",
        labels.join(", ")
    ))
}

pub(crate) fn modified_single_file_variants<'a>(
    cache_text: &str,
    variants: &'a [SingleFileVariant],
) -> Vec<&'a SingleFileVariant> {
    variants
        .iter()
        .filter(|variant| variant.content != cache_text)
        .collect()
}

pub(crate) fn format_single_file_diff(diff: &SingleFileDiff<'_>) -> String {
    similar::TextDiff::from_lines(diff.upstream, diff.installed_text)
        .unified_diff()
        .context_radius(3)
        .header(
            &format!("a/{}.md (upstream sha={})", diff.entry_name, diff.sha),
            &format!(
                "b/{}.md (installed: {})",
                diff.entry_name, diff.target_label
            ),
        )
        .to_string()
}

pub(crate) fn modified_dir_content(
    cache_files: &BTreeMap<String, PathBuf>,
    variant: &DirVariant,
) -> Result<DirContentMap, SkillfileError> {
    let mut modified = BTreeMap::new();
    for (filename, cache_file) in cache_files {
        let Some(installed) = variant.files.get(filename) else {
            continue;
        };
        if !installed.exists() {
            continue;
        }
        let cache_text = std::fs::read_to_string(cache_file)?;
        let installed_text = std::fs::read_to_string(installed)?;
        if installed_text != cache_text {
            modified.insert(filename.clone(), installed_text);
        }
    }
    Ok(modified)
}

pub(crate) fn modified_dir_variants(
    cache_files: &BTreeMap<String, PathBuf>,
    variants: &[DirVariant],
) -> Result<Vec<(String, DirContentMap)>, SkillfileError> {
    let mut modified = Vec::new();
    for variant in variants {
        let changed = modified_dir_content(cache_files, variant)?;
        if !changed.is_empty() {
            modified.push((variant.label.clone(), changed));
        }
    }
    Ok(modified)
}
