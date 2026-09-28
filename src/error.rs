use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("cannot find the home directory; set TRAIL_HOME or the XDG_*_HOME variables")]
    NoHome,
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write {path}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("scheduling failed: {0}")]
    Schedule(String),
    #[error("invalid config {path}")]
    Config {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error(
        "invalid range `{0}`; use today, yesterday, week, last-week, 2026-09-25, 2026-W39 or FROM..TO"
    )]
    Range(String),
    #[error("date error")]
    Time(#[from] jiff::Error),
    #[error("database error")]
    Db(#[from] rusqlite::Error),
}
