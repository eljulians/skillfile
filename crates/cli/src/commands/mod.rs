pub mod add;
pub mod add_tui;
pub mod diff;
pub mod format;
pub mod info;
pub mod init;
mod installed_variants;
mod multi_target;
pub mod pin;
pub mod remove;
pub mod resolve;
pub mod search;
pub mod search_tui;
pub mod skill_preview;
pub mod status;
pub mod validate;

pub(crate) fn forward_slash(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::forward_slash;

    #[test]
    fn forward_slash_keeps_unix_paths() {
        assert_eq!(
            forward_slash(std::path::Path::new("scripts/helper.py")),
            "scripts/helper.py"
        );
    }

    #[test]
    fn forward_slash_normalizes_windows_paths() {
        assert_eq!(
            forward_slash(std::path::Path::new(r"scripts\helper.py")),
            "scripts/helper.py"
        );
    }
}
