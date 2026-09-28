mod config;
mod discover;
mod error;
mod git;
mod paths;
mod range;
mod reflog;
mod schedule;
mod store;
mod view;

use std::fs;
use std::path::Path;

use anyhow::Context;
use clap::{Parser, Subcommand};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::config::Config;
use crate::discover::Repo;
use crate::error::AppError;
use crate::paths::Paths;
use crate::store::{LogState, Row, Store};

/// A local record of every commit you make on this machine.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan every repo under the configured roots for new commits.
    Scan {
        /// Write nothing to stdout.
        #[arg(long)]
        quiet: bool,
    },
    /// Show commits in a date range.
    Show {
        /// today, yesterday, week, last-week, 2026-09-25, 2026-W39 or FROM..TO.
        #[arg(default_value = "today")]
        range: String,
        /// Only commits from repos whose path contains this.
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        json: bool,
        /// Skip the scan that runs first.
        #[arg(long)]
        no_scan: bool,
    },
    /// Full-text search over subjects, bodies and changed paths.
    Search {
        query: String,
        /// Only commits from repos whose path contains this.
        #[arg(long)]
        repo: Option<String>,
        /// Only commits on or after this date.
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        json: bool,
        /// Skip the scan that runs first.
        #[arg(long)]
        no_scan: bool,
    },
    /// List the repos trail knows about.
    Repos,
    /// Register the scheduled scan.
    Install,
    /// Remove the scheduled scan.
    Uninstall,
    /// Add commits from git history that reflogs no longer hold (expired, fresh clone).
    Backfill {
        /// Author email to include; repeat for each address you commit with.
        #[arg(long = "author", required = true)]
        authors: Vec<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let home = std::env::home_dir();
    let paths = paths::resolve(home.as_deref(), |key| std::env::var_os(key).map(Into::into))?;
    init_logging(&paths.state)?;
    let result = run(&cli.command, &paths, home.as_deref());
    if let Err(e) = &result {
        tracing::error!("{e:#}");
    }
    result
}

fn run(command: &Command, paths: &Paths, home: Option<&Path>) -> anyhow::Result<()> {
    let config = config::load(&paths.config.join("config.toml"))?;
    match command {
        Command::Scan { quiet } => {
            let mut store = open_store(paths)?;
            let (repos, commits) = scan(&mut store, &config, home)?;
            if !*quiet {
                println!("{repos} repos, {commits} new commits");
            }
        }
        Command::Show {
            range,
            repo,
            json,
            no_scan,
        } => {
            let mut store = open_store(paths)?;
            if !*no_scan {
                scan(&mut store, &config, home)?;
            }
            let tz = TimeZone::system();
            let (from, to) = range::parse(range, Timestamp::now().to_zoned(tz.clone()).date())?;
            let (start, end) = range::span(from, to, &tz)?;
            let rows = view::dedupe(store.commits_between(start, end, repo.as_deref())?);
            print_rows(&rows, *json, &tz)?;
        }
        Command::Search {
            query,
            repo,
            since,
            json,
            no_scan,
        } => {
            let mut store = open_store(paths)?;
            if !*no_scan {
                scan(&mut store, &config, home)?;
            }
            let tz = TimeZone::system();
            let since = since
                .as_deref()
                .map(|s| -> anyhow::Result<_> {
                    let (from, _) = range::parse(s, Timestamp::now().to_zoned(tz.clone()).date())?;
                    Ok(range::span(from, from, &tz)?.0)
                })
                .transpose()?;
            let rows = view::dedupe(store.search(query, repo.as_deref(), since)?);
            print_rows(&rows, *json, &tz)?;
        }
        Command::Repos => {
            let store = open_store(paths)?;
            print!(
                "{}",
                view::repos_text(&store.repo_summaries()?, &TimeZone::system())
            );
        }
        Command::Install => {
            print!(
                "{}",
                schedule::install(&std::env::current_exe()?, &paths.state)?
            );
        }
        Command::Uninstall => print!("{}", schedule::uninstall()?),
        Command::Backfill { authors } => {
            let mut store = open_store(paths)?;
            let (repos, commits) = backfill(&mut store, &config, home, authors)?;
            println!("{repos} repos, {commits} new commits");
        }
    }
    Ok(())
}

fn print_rows(rows: &[Row], json: bool, tz: &TimeZone) -> anyhow::Result<()> {
    if json {
        println!("{}", view::json(rows, tz)?);
    } else {
        print!("{}", view::text(rows, tz));
    }
    Ok(())
}

/// Appends to `trail.log` in the state dir; nothing goes to stdout.
fn init_logging(state: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(state).with_context(|| format!("cannot create {}", state.display()))?;
    let log = tracing_appender::rolling::RollingFileAppender::builder()
        .filename_prefix("trail.log")
        .build(state)?;
    tracing_subscriber::fmt()
        .with_writer(log)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .init();
    Ok(())
}

fn open_store(paths: &Paths) -> anyhow::Result<Store> {
    fs::create_dir_all(&paths.data)
        .with_context(|| format!("cannot create {}", paths.data.display()))?;
    Ok(Store::open(&paths.data.join("trail.db"))?)
}

/// Stores every reachable commit by `authors` in every repo under the configured roots.
/// Returns (repos seen, new commits).
fn backfill(
    store: &mut Store,
    config: &Config,
    home: Option<&Path>,
    authors: &[String],
) -> Result<(usize, usize), AppError> {
    let repos = discover_repos(config, home);
    let mut new = 0usize;
    for repo in &repos {
        let path = repo.path.display().to_string();
        let commits = match git::log_by_authors(&repo.path, authors) {
            Ok(commits) => commits,
            Err(e) => {
                tracing::warn!(repo = %path, "git log failed: {e}");
                continue;
            }
        };
        let added = store.backfill(store.repo_id(&path)?, &commits)?;
        tracing::info!(repo = %path, added, "backfilled");
        new = new.saturating_add(added);
    }
    Ok((repos.len(), new))
}

fn discover_repos(config: &Config, home: Option<&Path>) -> Vec<Repo> {
    let roots: Vec<_> = config
        .roots
        .iter()
        .map(|r| config::expand_root(r, home))
        .collect();
    discover::discover(&roots, config.max_depth, &config.skip)
}

/// Scans every repo under the configured roots. Returns (repos seen, new commits).
fn scan(
    store: &mut Store,
    config: &Config,
    home: Option<&Path>,
) -> Result<(usize, usize), AppError> {
    let repos = discover_repos(config, home);
    let mut new = 0usize;
    for repo in &repos {
        new = new.saturating_add(scan_repo(store, repo)?);
    }
    tracing::info!(repos = repos.len(), new, "scan done");
    Ok((repos.len(), new))
}

/// One repo, one transaction. Git and file problems are logged and skipped; database
/// errors are returned.
fn scan_repo(store: &mut Store, repo: &Repo) -> Result<usize, AppError> {
    let path = repo.path.display().to_string();
    let repo_id = store.repo_id(&path)?;
    let known = store.log_states(repo_id)?;
    let mut states = Vec::new();
    let mut entries = Vec::new();
    for log in &repo.logs {
        let log_path = log.path.display().to_string();
        let (state, bytes) = match log_state(&log.path, &log_path) {
            Ok(state) if known.contains(&state) => continue,
            Ok(state) => match fs::read(&log.path) {
                Ok(bytes) => (state, bytes),
                Err(e) => {
                    tracing::warn!(log = %log_path, "cannot read reflog: {e}");
                    continue;
                }
            },
            Err(e) => {
                tracing::warn!(log = %log_path, "cannot stat reflog: {e}");
                continue;
            }
        };
        entries.extend(
            String::from_utf8_lossy(&bytes)
                .lines()
                .filter_map(reflog::parse_line)
                .filter(|e| reflog::creates_commit(&e.op, &e.message))
                .filter_map(|e| {
                    Some(store::Entry {
                        ref_name: log.ref_name.clone(),
                        hash: e.hash,
                        op: e.op,
                        ts: Timestamp::from_second(e.ts).ok()?,
                        message: e.message,
                    })
                }),
        );
        states.push(state);
    }
    if states.is_empty() {
        return Ok(0);
    }
    let mut hashes: Vec<_> = entries.iter().map(|e| e.hash.clone()).collect();
    hashes.sort_unstable();
    hashes.dedup();
    let commits: Vec<_> = store
        .missing_hashes(repo_id, &hashes)?
        .iter()
        .filter_map(|hash| {
            git::show(&repo.path, hash)
                .inspect_err(|e| tracing::warn!(repo = %path, hash, "git show failed: {e}"))
                .ok()
        })
        .collect();
    store.record(repo_id, &entries, &commits, &states)?;
    tracing::info!(repo = %path, new = commits.len(), "scanned");
    Ok(commits.len())
}

fn log_state(path: &Path, key: &str) -> std::io::Result<LogState> {
    let meta = fs::metadata(path)?;
    let mtime = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    Ok(LogState {
        path: key.to_owned(),
        size: i64::try_from(meta.len()).map_err(std::io::Error::other)?,
        mtime: i64::try_from(mtime).map_err(std::io::Error::other)?,
    })
}
