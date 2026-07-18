pub mod commands;
pub mod config;
pub mod patch;

/// Return whether an environment variable contains an enabled boolean flag.
///
/// Boolean flags are enabled only by the documented values `1` and `true`.
pub fn env_flag(name: &str) -> bool {
    env_value_is_true(std::env::var(name).ok().as_deref())
}

fn env_value_is_true(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "true"))
}

#[cfg(test)]
mod tests {
    use super::env_value_is_true;

    #[test]
    fn env_flags_require_explicit_true_values() {
        for value in [Some("1"), Some("true")] {
            assert!(env_value_is_true(value), "{value:?} should enable the flag");
        }

        for value in [
            None,
            Some(""),
            Some("0"),
            Some("false"),
            Some("yes"),
            Some("TRUE"),
        ] {
            assert!(
                !env_value_is_true(value),
                "{value:?} should not enable the flag"
            );
        }
    }
}
