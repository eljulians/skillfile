use std::collections::HashMap;
use std::path::{Path, PathBuf};

use skillfile_core::error::SkillfileError;
use skillfile_core::models::{EntityType, Entry, InstallTarget, Scope};

use crate::adapter::{
    adapters, AdapterScope, DirInstallMode, EntityConfig, FileSystemAdapter, PlatformAdapter,
};

pub enum ResolvedInstallTarget<'a> {
    BuiltIn {
        adapter: &'a dyn PlatformAdapter,
        scope: Scope,
    },
    Explicit {
        adapter: FileSystemAdapter,
    },
}

impl<'a> ResolvedInstallTarget<'a> {
    pub fn from_target(target: &'a InstallTarget) -> Result<Self, SkillfileError> {
        match target {
            InstallTarget::Platform { adapter, scope } => Ok(Self::BuiltIn {
                adapter: built_in_adapter(adapter)?,
                scope: *scope,
            }),
            InstallTarget::Path {
                tool_name,
                entity_type,
                path,
            } => Ok(Self::Explicit {
                adapter: explicit_adapter(tool_name, *entity_type, path),
            }),
        }
    }

    pub fn supports(&self, entity_type: EntityType) -> bool {
        self.adapter().supports(entity_type)
    }

    pub fn scope(&self) -> Scope {
        match self {
            Self::BuiltIn { scope, .. } => *scope,
            Self::Explicit { .. } => Scope::Global,
        }
    }

    pub fn adapter(&self) -> &dyn PlatformAdapter {
        match self {
            Self::BuiltIn { adapter, .. } => *adapter,
            Self::Explicit { adapter } => adapter,
        }
    }

    pub fn adapter_scope<'b>(&self, repo_root: &'b Path) -> AdapterScope<'b> {
        AdapterScope {
            scope: self.scope(),
            repo_root,
        }
    }

    pub fn target_dir(&self, entity_type: EntityType, repo_root: &Path) -> PathBuf {
        let scope = self.adapter_scope(repo_root);
        self.adapter().target_dir(entity_type, &scope)
    }

    pub fn dir_mode(&self, entity_type: EntityType) -> Option<DirInstallMode> {
        self.adapter().dir_mode(entity_type)
    }

    pub fn installed_path(&self, entry: &Entry, repo_root: &Path) -> PathBuf {
        let scope = self.adapter_scope(repo_root);
        self.adapter().installed_path(entry, &scope)
    }

    pub fn installed_dir_files(&self, entry: &Entry, repo_root: &Path) -> HashMap<String, PathBuf> {
        let scope = self.adapter_scope(repo_root);
        self.adapter().installed_dir_files(entry, &scope)
    }
}

fn built_in_adapter(adapter: &str) -> Result<&'static dyn PlatformAdapter, SkillfileError> {
    adapters()
        .get(adapter)
        .ok_or_else(|| SkillfileError::Manifest(format!("unknown adapter '{adapter}'")))
}

fn explicit_adapter(tool_name: &str, entity_type: EntityType, path: &str) -> FileSystemAdapter {
    let mut entities = HashMap::new();
    entities.insert(
        entity_type,
        EntityConfig {
            global_path: path.to_string(),
            local_path: path.to_string(),
            dir_mode: default_dir_mode(entity_type),
        },
    );
    FileSystemAdapter::new(tool_name, entities).preserve_legacy_flat_files()
}

fn default_dir_mode(entity_type: EntityType) -> DirInstallMode {
    match entity_type {
        EntityType::Skill => DirInstallMode::Nested,
        EntityType::Agent => DirInstallMode::Flat,
    }
}
