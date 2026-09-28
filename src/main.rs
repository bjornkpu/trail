mod claude;
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
use crate::store::{LogState, Store};

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
    /// Show commits and Claude sessions in a date range.
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
        /// List each Claude session's prompts under it.
        #[arg(long)]
        prompts: bool,
    },
    /// Full-text search over commit subjects, bodies, changed paths and Claude prompts.
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
    let claude = paths::claude_dir(home, |key| std::env::var_os(key).map(Into::into));
    match command {
        Command::Scan { quiet } => {
            let mut store = open_store(paths)?;
            let s = scan(&mut store, &config, home, claude.as_deref())?;
            if !*quiet {
                println!(
                    "{} repos, {} new commits, {} sessions, {} new prompts",
                    s.repos, s.commits, s.sessions, s.prompts
                );
            }
        }
        Command::Show {
            range,
            repo,
            json,
            no_scan,
            prompts,
        } => {
            let mut store = open_store(paths)?;
            if !*no_scan {
                scan(&mut store, &config, home, claude.as_deref())?;
            }
            let tz = TimeZone::system();
            let (from, to) = range::parse(range, Timestamp::now().to_zoned(tz.clone()).date())?;
            let (start, end) = range::span(from, to, &tz)?;
            let commits = view::dedupe(store.commits_between(start, end, repo.as_deref())?);
            let sessions = store.sessions_between(start, end)?;
            let items = view::items(commits, sessions, &store.repo_paths()?, repo.as_deref());
            print_items(&items, *json, &tz, *prompts)?;
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
                scan(&mut store, &config, home, claude.as_deref())?;
            }
            let tz = TimeZone::system();
            let since = since
                .as_deref()
                .map(|s| -> anyhow::Result<_> {
                    let (from, _) = range::parse(s, Timestamp::now().to_zoned(tz.clone()).date())?;
                    Ok(range::span(from, from, &tz)?.0)
                })
                .transpose()?;
            let commits = view::dedupe(store.search(query, repo.as_deref(), since)?);
            let sessions = store.search_sessions(query, since)?;
            let items = view::items(commits, sessions, &store.repo_paths()?, repo.as_deref());
            print_items(&items, *json, &tz, true)?;
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

fn print_items(
    items: &[view::Item],
    json: bool,
    tz: &TimeZone,
    prompts: bool,
) -> anyhow::Result<()> {
    if json {
        println!("{}", view::json(items, tz)?);
    } else {
        print!("{}", view::text(items, tz, prompts));
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

/// What one scan found.
struct Scanned {
    repos: usize,
    commits: usize,
    /// Session files read because they changed since the last scan.
    sessions: usize,
    prompts: usize,
}

/// Scans every repo under the configured roots, then Claude Code's session files.
fn scan(
    store: &mut Store,
    config: &Config,
    home: Option<&Path>,
    claude: Option<&Path>,
) -> Result<Scanned, AppError> {
    let repos = discover_repos(config, home);
    let mut commits = 0usize;
    for repo in &repos {
        commits = commits.saturating_add(scan_repo(store, repo)?);
    }
    let (sessions, prompts) = match claude {
        Some(dir) => scan_sessions(store, dir)?,
        None => (0, 0),
    };
    tracing::info!(repos = repos.len(), commits, sessions, prompts, "scan done");
    Ok(Scanned {
        repos: repos.len(),
        commits,
        sessions,
        prompts,
    })
}

/// Reads each changed `projects/*/*.jsonl` under `claude`. Subagent transcripts sit one
/// level deeper and are never reached. A missing directory means no sessions. Returns
/// (files read, new prompts).
fn scan_sessions(store: &mut Store, claude: &Path) -> Result<(usize, usize), AppError> {
    let Ok(projects) = fs::read_dir(claude.join("projects")) else {
        return Ok((0, 0));
    };
    let known = store.session_states()?;
    let files = projects
        .flatten()
        .filter_map(|project| fs::read_dir(project.path()).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"));
    let (mut read, mut prompts) = (0usize, 0usize);
    // ponytail: re-reads a changed file whole; store a byte offset if scans get slow.
    for file in files {
        let key = file.display().to_string();
        let state = match log_state(&file, &key) {
            Ok(state) if known.contains(&state) => continue,
            Ok(state) => state,
            Err(e) => {
                tracing::warn!(file = %key, "cannot stat session: {e}");
                continue;
            }
        };
        let bytes = match fs::read(&file) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(file = %key, "cannot read session: {e}");
                continue;
            }
        };
        let parsed = claude::parse(&String::from_utf8_lossy(&bytes));
        prompts = prompts.saturating_add(store.record_session(&parsed, &state)?);
        read = read.saturating_add(1);
    }
    Ok((read, prompts))
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
