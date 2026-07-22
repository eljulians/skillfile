use std::collections::HashMap;
use std::path::Path;

use skillfile_core::error::SkillfileError;
use skillfile_core::models::{Entry, Manifest};
use skillfile_deploy::paths::is_safe_installed_path;
use skillfile_deploy::target::ResolvedInstallTarget;

pub(crate) struct SingleFileVariant {
    pub(crate) label: String,
    pub(crate) content: String,
}

pub(crate) struct DirVariant {
    pub(crate) label: String,
    pub(crate) files: HashMap<String, std::path::PathBuf>,
}

pub(crate) fn installed_single_file_variants(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Result<Vec<SingleFileVariant>, SkillfileError> {
    let mut variants = Vec::new();

    for target in &manifest.install_targets {
        let Ok(resolved) = ResolvedInstallTarget::from_target(target) else {
            continue;
        };
        if !resolved.supports(entry.entity_type) {
            continue;
        }

        let path = resolved.installed_path(entry, repo_root);
        if !is_safe_installed_path(&path) || !path.exists() {
            continue;
        }

        variants.push(SingleFileVariant {
            label: target.to_string(),
            content: std::fs::read_to_string(path)?,
        });
    }

    Ok(variants)
}

pub(crate) fn installed_dir_variants(
    entry: &Entry,
    manifest: &Manifest,
    repo_root: &Path,
) -> Vec<DirVariant> {
    let mut variants = Vec::new();

    for target in &manifest.install_targets {
        let Ok(resolved) = ResolvedInstallTarget::from_target(target) else {
            continue;
        };
        if !resolved.supports(entry.entity_type) {
            continue;
        }

        let files = resolved.installed_dir_files(entry, repo_root);
        if files.is_empty() {
            continue;
        }

        variants.push(DirVariant {
            label: target.to_string(),
            files,
        });
    }

    variants
}
