use std::path::{Path, PathBuf};

use crate::error::AppError;

/// Directories trail reads from and writes to. Each already ends in `trail`,
/// unless `TRAIL_HOME` put all three in one place.
#[derive(Debug, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
}

/// Resolves trail's directories from `home` and an environment lookup.
/// `TRAIL_HOME` wins over everything. Empty or relative `XDG_*_HOME` values are
/// ignored, as the XDG spec asks.
pub fn resolve(
    home: Option<&Path>,
    var: impl Fn(&str) -> Option<PathBuf>,
) -> Result<Paths, AppError> {
    if let Some(dir) = var("TRAIL_HOME").filter(|p| !p.as_os_str().is_empty()) {
        return Ok(Paths {
            config: dir.clone(),
            data: dir.clone(),
            state: dir,
        });
    }
    let xdg = |key: &str, fallback: &[&str]| {
        var(key)
            .filter(|p| p.is_absolute())
            .or_else(|| home.map(|h| fallback.iter().fold(h.to_path_buf(), |p, c| p.join(c))))
            .map(|base| base.join("trail"))
            .ok_or(AppError::NoHome)
    };
    Ok(Paths {
        config: xdg("XDG_CONFIG_HOME", &[".config"])?,
        data: xdg("XDG_DATA_HOME", &[".local", "share"])?,
        state: xdg("XDG_STATE_HOME", &[".local", "state"])?,
    })
}

/// Claude Code's config directory: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_dir(home: Option<&Path>, var: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    var("CLAUDE_CONFIG_DIR")
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| home.map(|h| h.join(".claude")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<PathBuf> + 'a {
        |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| PathBuf::from(v))
        }
    }

    // Absolute on both Windows and Unix.
    fn abs(p: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{p}"))
        } else {
            PathBuf::from(p)
        }
    }

    #[test]
    fn defaults_under_home() {
        let home = abs("/home/bk");
        let paths = resolve(Some(&home), env(&[])).unwrap();
        assert_eq!(
            paths,
            Paths {
                config: home.join(".config").join("trail"),
                data: home.join(".local").join("share").join("trail"),
                state: home.join(".local").join("state").join("trail"),
            }
        );
    }

    #[test]
    fn xdg_vars_override_each_dir() {
        let (c, d, s) = (abs("/xc"), abs("/xd"), abs("/xs"));
        let vars = [
            ("XDG_CONFIG_HOME", c.to_str().unwrap()),
            ("XDG_DATA_HOME", d.to_str().unwrap()),
            ("XDG_STATE_HOME", s.to_str().unwrap()),
        ];
        let paths = resolve(None, env(&vars)).unwrap();
        assert_eq!(
            paths,
            Paths {
                config: c.join("trail"),
                data: d.join("trail"),
                state: s.join("trail"),
            }
        );
    }

    #[test]
    fn empty_or_relative_xdg_var_is_ignored() {
        let home = abs("/home/bk");
        let vars = [("XDG_CONFIG_HOME", ""), ("XDG_DATA_HOME", "rel/dir")];
        let paths = resolve(Some(&home), env(&vars)).unwrap();
        assert_eq!(paths.config, home.join(".config").join("trail"));
        assert_eq!(paths.data, home.join(".local").join("share").join("trail"));
    }

    #[test]
    fn trail_home_overrides_all_three() {
        let t = abs("/tmp/t");
        let vars = [
            ("TRAIL_HOME", t.to_str().unwrap()),
            ("XDG_CONFIG_HOME", "/xc"),
        ];
        let paths = resolve(None, env(&vars)).unwrap();
        assert_eq!(
            paths,
            Paths {
                config: t.clone(),
                data: t.clone(),
                state: t,
            }
        );
    }

    #[test]
    fn empty_trail_home_is_ignored() {
        let home = abs("/home/bk");
        let paths = resolve(Some(&home), env(&[("TRAIL_HOME", "")])).unwrap();
        assert_eq!(paths.config, home.join(".config").join("trail"));
    }

    #[test]
    fn no_home_and_no_vars_is_an_error() {
        assert!(matches!(resolve(None, env(&[])), Err(AppError::NoHome)));
    }

    #[test]
    fn claude_dir_defaults_under_home() {
        let home = abs("/home/bk");
        assert_eq!(
            claude_dir(Some(&home), env(&[])),
            Some(home.join(".claude"))
        );
    }

    #[test]
    fn claude_config_dir_overrides_home() {
        let c = abs("/c");
        let vars = [("CLAUDE_CONFIG_DIR", c.to_str().unwrap())];
        assert_eq!(claude_dir(Some(&abs("/home/bk")), env(&vars)), Some(c));
    }

    #[test]
    fn empty_claude_config_dir_is_ignored() {
        let home = abs("/home/bk");
        let vars = [("CLAUDE_CONFIG_DIR", "")];
        assert_eq!(
            claude_dir(Some(&home), env(&vars)),
            Some(home.join(".claude"))
        );
        assert_eq!(claude_dir(None, env(&[])), None);
    }
}
