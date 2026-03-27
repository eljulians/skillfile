use thiserror::Error;

/// Root error type for the skillfile domain.
///
/// Library crates use this typed enum so callers can match on error variants.
/// The CLI binary wraps these in `anyhow::Error` for top-level reporting.
#[derive(Error, Debug)]
pub enum SkillfileError {
    #[error("{0}")]
    Manifest(String),

    #[error("{0}")]
    Network(String),

    #[error("{0}")]
    Install(String),

    #[error("{message}")]
    PatchConflict { message: String, entry_name: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, SkillfileError>;
