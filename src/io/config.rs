use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::AppError;

/// Contents of the optional `config.toml`. Missing keys take their defaults.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub roots: Vec<String>,
    pub max_depth: usize,
    pub skip: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roots: vec!["~".into()],
            max_depth: 4,
            skip: ["AppData", ".cache", "target", "node_modules"]
                .map(String::from)
                .to_vec(),
        }
    }
}

/// Expands a leading `~` in a configured root to `home`.
#[must_use]
pub fn expand_root(root: &str, home: Option<&Path>) -> PathBuf {
    let rest = root
        .strip_prefix('~')
        .filter(|r| r.is_empty() || r.starts_with(['/', '\\']));
    match (rest, home) {
        (Some(rest), Some(home)) => home.join(rest.trim_start_matches(['/', '\\'])),
        _ => PathBuf::from(root),
    }
}

fn parse(text: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(text)
}

/// Reads the config at `path`, or returns the defaults when the file is absent.
pub fn load(path: &Path) -> Result<Config, AppError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(source) => {
            return Err(AppError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    parse(&text).map_err(|source| AppError::Config {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_defaults() {
        let config = parse("").unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(config.roots, ["~"]);
        assert_eq!(config.max_depth, 4);
        assert_eq!(config.skip, ["AppData", ".cache", "target", "node_modules"]);
    }

    #[test]
    fn set_keys_override_and_missing_keys_default() {
        let config = parse("roots = ['~/src', 'D:/work']\nmax_depth = 2").unwrap();
        assert_eq!(config.roots, ["~/src", "D:/work"]);
        assert_eq!(config.max_depth, 2);
        assert_eq!(config.skip, Config::default().skip);
    }

    #[test]
    fn unknown_key_is_rejected() {
        assert!(parse("max_dept = 2").is_err());
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = Path::new("/home/bk");
        assert_eq!(expand_root("~", Some(home)), home);
        assert_eq!(expand_root("~/src", Some(home)), home.join("src"));
        assert_eq!(expand_root("~\\src", Some(home)), home.join("src"));
        assert_eq!(expand_root("D:/work", Some(home)), Path::new("D:/work"));
        assert_eq!(expand_root("~bob", Some(home)), Path::new("~bob"));
        assert_eq!(expand_root("~", None), Path::new("~"));
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = std::env::temp_dir().join("trail-config-test-missing");
        assert_eq!(load(&dir.join("nope.toml")).unwrap(), Config::default());
    }

    #[test]
    fn invalid_file_error_names_the_path() {
        let path = std::env::temp_dir().join("trail-config-test-invalid.toml");
        std::fs::write(&path, "max_depth = 'deep'").unwrap();
        let err = load(&path).unwrap_err();
        assert!(err.to_string().contains("trail-config-test-invalid.toml"));
    }
}
