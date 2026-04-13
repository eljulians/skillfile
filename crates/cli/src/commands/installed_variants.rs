use std::collections::HashMap;
use std::path::Path;

use skillfile_core::error::SkillfileError;
use skillfile_core::models::{Entry, Manifest};
use skillfile_deploy::adapter::{adapters, AdapterScope};

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
    let all_adapters = adapters();
    let mut variants = Vec::new();

    for target in &manifest.install_targets {
        let Some(adapter) = all_adapters.get(&target.adapter) else {
            continue;
        };
        if !adapter.supports(entry.entity_type) {
            continue;
        }

        let path = adapter.installed_path(
            entry,
            &AdapterScope {
                scope: target.scope,
                repo_root,
            },
        );
        if !path.exists() {
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
    let all_adapters = adapters();
    let mut variants = Vec::new();

    for target in &manifest.install_targets {
        let Some(adapter) = all_adapters.get(&target.adapter) else {
            continue;
        };
        if !adapter.supports(entry.entity_type) {
            continue;
        }

        let files = adapter.installed_dir_files(
            entry,
            &AdapterScope {
                scope: target.scope,
                repo_root,
            },
        );
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
