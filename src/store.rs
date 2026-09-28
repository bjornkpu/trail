use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};

use crate::claude::{Kind, Parsed, Prompt, Session};
use crate::error::AppError;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS repos (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    first_seen TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS repo_state (
    repo_id INTEGER NOT NULL REFERENCES repos(id),
    log_path TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    PRIMARY KEY (repo_id, log_path)
);
CREATE TABLE IF NOT EXISTS reflog_entries (
    repo_id INTEGER NOT NULL REFERENCES repos(id),
    ref TEXT NOT NULL,
    hash TEXT NOT NULL,
    op TEXT NOT NULL,
    ts TEXT NOT NULL,
    message TEXT NOT NULL,
    UNIQUE (repo_id, ref, hash, ts, op)
);
CREATE INDEX IF NOT EXISTS reflog_entries_hash ON reflog_entries (repo_id, hash);
CREATE TABLE IF NOT EXISTS commits (
    repo_id INTEGER NOT NULL REFERENCES repos(id),
    hash TEXT NOT NULL,
    author_name TEXT NOT NULL,
    author_email TEXT NOT NULL,
    author_date TEXT NOT NULL,
    committer_date TEXT NOT NULL,
    subject TEXT NOT NULL,
    body TEXT NOT NULL,
    source TEXT NOT NULL,
    PRIMARY KEY (repo_id, hash)
);
CREATE INDEX IF NOT EXISTS commits_author_date ON commits (author_date);
CREATE TABLE IF NOT EXISTS commit_files (
    repo_id INTEGER NOT NULL,
    hash TEXT NOT NULL,
    path TEXT NOT NULL,
    insertions INTEGER,
    deletions INTEGER,
    PRIMARY KEY (repo_id, hash, path)
);
CREATE VIRTUAL TABLE IF NOT EXISTS commits_fts USING fts5 (subject, body, paths);
CREATE TRIGGER IF NOT EXISTS commits_fts_insert AFTER INSERT ON commits BEGIN
    INSERT INTO commits_fts (rowid, subject, body, paths)
    VALUES (new.rowid, new.subject, new.body, '');
END;
CREATE TRIGGER IF NOT EXISTS commit_files_fts_insert AFTER INSERT ON commit_files BEGIN
    UPDATE commits_fts SET paths = paths || ' ' || new.path
    WHERE rowid = (SELECT rowid FROM commits WHERE repo_id = new.repo_id AND hash = new.hash);
END;
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    cwd TEXT NOT NULL,
    git_branch TEXT NOT NULL,
    title TEXT,
    started TEXT NOT NULL,
    ended TEXT NOT NULL,
    model TEXT,
    cost_usd REAL,
    lines_added INTEGER,
    lines_removed INTEGER,
    cc_version TEXT
);
CREATE TABLE IF NOT EXISTS prompts (
    uuid TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    ts TEXT NOT NULL,
    kind TEXT NOT NULL,
    text TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS prompts_session ON prompts (session_id, ts);
CREATE TABLE IF NOT EXISTS session_state (
    path TEXT PRIMARY KEY,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS prompts_fts USING fts5 (text);
CREATE TRIGGER IF NOT EXISTS prompts_fts_insert AFTER INSERT ON prompts BEGIN
    INSERT INTO prompts_fts (rowid, text) VALUES (new.rowid, new.text);
END;
";

pub struct Store {
    conn: Connection,
}

/// Size and mtime of a reflog file when it was last scanned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogState {
    pub path: String,
    pub size: i64,
    /// Nanoseconds since the Unix epoch.
    pub mtime: i64,
}

/// A kept reflog entry.
#[derive(Debug)]
pub struct Entry {
    pub ref_name: String,
    pub hash: String,
    pub op: String,
    pub ts: Timestamp,
    pub message: String,
}

#[derive(Debug)]
pub struct Commit {
    pub hash: String,
    pub author_name: String,
    pub author_email: String,
    pub author_date: Timestamp,
    pub committer_date: Timestamp,
    pub subject: String,
    pub body: String,
    pub files: Vec<FileStat>,
}

/// One `git diff-tree --numstat` line. Binary files have no line counts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FileStat {
    pub path: String,
    pub insertions: Option<i64>,
    pub deletions: Option<i64>,
}

/// A stored commit with the repo path and the branch it was made on.
#[derive(Debug)]
pub struct Row {
    pub repo: String,
    /// Branch name without `refs/heads/`, or the ref when no branch log has the commit.
    pub branch: String,
    /// How the commit got here: `reflog` (made in this clone) or `backfill`.
    pub source: String,
    pub commit: Commit,
}

/// A stored session with the prompts a query asked for.
#[derive(Debug)]
pub struct SessionRow {
    pub session: Session,
    pub prompts: Vec<Prompt>,
}

/// UTC ISO 8601 with second precision, so stored dates sort as text.
fn iso(ts: Timestamp) -> String {
    ts.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[derive(Debug, PartialEq, Eq)]
pub struct RepoSummary {
    pub path: String,
    pub commits: i64,
    pub last: Option<Timestamp>,
}

/// Quotes each word as an FTS5 string, so `-`, `:` and quotes are plain text and all
/// words must match.
fn fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Inserts commits not yet stored, with their files. Returns how many were new.
fn insert_commits(
    tx: &rusqlite::Transaction,
    repo_id: i64,
    commits: &[Commit],
    source: &str,
) -> Result<usize, AppError> {
    let mut commit_stmt = tx.prepare(
        "INSERT OR IGNORE INTO commits (repo_id, hash, author_name, author_email,
         author_date, committer_date, subject, body, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    let mut file_stmt = tx.prepare(
        "INSERT OR IGNORE INTO commit_files (repo_id, hash, path, insertions, deletions)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut added = 0usize;
    for c in commits {
        let inserted = commit_stmt.execute(params![
            repo_id,
            c.hash,
            c.author_name,
            c.author_email,
            iso(c.author_date),
            iso(c.committer_date),
            c.subject,
            c.body,
            source,
        ])?;
        if inserted == 0 {
            continue;
        }
        added = added.saturating_add(1);
        for f in &c.files {
            file_stmt.execute(params![repo_id, c.hash, f.path, f.insertions, f.deletions])?;
        }
    }
    Ok(added)
}

fn timestamp(row: &rusqlite::Row, idx: usize) -> rusqlite::Result<Timestamp> {
    let text: String = row.get(idx)?;
    text.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
    })
}

impl Store {
    /// Opens or creates the database. WAL and a busy timeout let a scheduled scan and an
    /// interactive command overlap.
    pub fn open(path: &Path) -> Result<Self, AppError> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// The id of the repo at `path`, added on first sight.
    pub fn repo_id(&self, path: &str) -> Result<i64, AppError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO repos (path, first_seen) VALUES (?1, ?2)",
            params![path, iso(Timestamp::now())],
        )?;
        Ok(self
            .conn
            .query_row("SELECT id FROM repos WHERE path = ?1", [path], |r| r.get(0))?)
    }

    pub fn log_states(&self, repo_id: i64) -> Result<Vec<LogState>, AppError> {
        let mut stmt = self.conn.prepare(
            "SELECT log_path, size, mtime FROM repo_state WHERE repo_id = ?1 ORDER BY log_path",
        )?;
        let rows = stmt.query_map([repo_id], |r| {
            Ok(LogState {
                path: r.get(0)?,
                size: r.get(1)?,
                mtime: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The hashes in `hashes` with no stored commit, in order.
    pub fn missing_hashes(&self, repo_id: i64, hashes: &[String]) -> Result<Vec<String>, AppError> {
        let mut stmt = self
            .conn
            .prepare("SELECT 1 FROM commits WHERE repo_id = ?1 AND hash = ?2")?;
        let mut missing = Vec::new();
        for hash in hashes {
            if stmt
                .query_row(params![repo_id, hash], |_| Ok(()))
                .optional()?
                .is_none()
            {
                missing.push(hash.clone());
            }
        }
        Ok(missing)
    }

    /// Commits authored in `[from, to)`, oldest first, optionally only from repos whose path
    /// contains `repo` (case-insensitive).
    pub fn commits_between(
        &self,
        from: Timestamp,
        to: Timestamp,
        repo: Option<&str>,
    ) -> Result<Vec<Row>, AppError> {
        self.rows(
            "WHERE c.author_date >= ?1 AND c.author_date < ?2
             AND (?3 IS NULL OR instr(lower(r.path), lower(?3)) > 0)
             ORDER BY c.author_date",
            params![iso(from), iso(to), repo],
        )
    }

    /// Commits whose subject, body or paths hold every word of `query`, oldest first.
    pub fn search(
        &self,
        query: &str,
        repo: Option<&str>,
        since: Option<Timestamp>,
    ) -> Result<Vec<Row>, AppError> {
        let fts = fts_query(query);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        self.rows(
            "WHERE c.rowid IN (SELECT rowid FROM commits_fts WHERE commits_fts MATCH ?1)
             AND (?2 IS NULL OR instr(lower(r.path), lower(?2)) > 0)
             AND (?3 IS NULL OR c.author_date >= ?3)
             ORDER BY c.author_date",
            params![fts, repo, since.map(iso)],
        )
    }

    /// Every known repo with its commit count and newest author date, by path.
    pub fn repo_summaries(&self) -> Result<Vec<RepoSummary>, AppError> {
        let mut stmt = self.conn.prepare(
            "SELECT r.path, count(c.hash), max(c.author_date)
             FROM repos r LEFT JOIN commits c ON c.repo_id = r.id
             GROUP BY r.id ORDER BY r.path",
        )?;
        let rows = stmt.query_map([], |r| {
            let last: Option<String> = r.get(2)?;
            Ok(RepoSummary {
                path: r.get(0)?,
                commits: r.get(1)?,
                last: last.map(|_| timestamp(r, 2)).transpose()?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Runs the shared commit query with `tail` (WHERE and ORDER BY) and loads each
    /// commit's files. The branch prefers a `refs/heads/` log over HEAD logs.
    fn rows(&self, tail: &str, params: impl rusqlite::Params) -> Result<Vec<Row>, AppError> {
        let sql = format!(
            "SELECT r.path, c.repo_id, c.hash, c.author_name, c.author_email, c.author_date,
                    c.committer_date, c.subject, c.body,
                    COALESCE((SELECT e.ref FROM reflog_entries e
                              WHERE e.repo_id = c.repo_id AND e.hash = c.hash
                              ORDER BY e.ref NOT LIKE 'refs/heads/%', e.ts LIMIT 1), 'HEAD'),
                    c.source
             FROM commits c JOIN repos r ON r.id = c.repo_id {tail}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut files = self.conn.prepare(
            "SELECT path, insertions, deletions FROM commit_files
             WHERE repo_id = ?1 AND hash = ?2 ORDER BY rowid",
        )?;
        let found = stmt.query_map(params, |r| {
            let branch: String = r.get(9)?;
            Ok((
                r.get::<_, i64>(1)?,
                Row {
                    repo: r.get(0)?,
                    branch: branch.strip_prefix("refs/heads/").unwrap_or(&branch).into(),
                    source: r.get(10)?,
                    commit: Commit {
                        hash: r.get(2)?,
                        author_name: r.get(3)?,
                        author_email: r.get(4)?,
                        author_date: timestamp(r, 5)?,
                        committer_date: timestamp(r, 6)?,
                        subject: r.get(7)?,
                        body: r.get(8)?,
                        files: Vec::new(),
                    },
                },
            ))
        })?;
        let mut rows = Vec::new();
        for item in found {
            let (repo_id, mut row) = item?;
            row.commit.files = files
                .query_map(params![repo_id, row.commit.hash], |r| {
                    Ok(FileStat {
                        path: r.get(0)?,
                        insertions: r.get(1)?,
                        deletions: r.get(2)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            rows.push(row);
        }
        Ok(rows)
    }

    /// Adds commits found in git history that are not stored yet. Returns how many.
    pub fn backfill(&mut self, repo_id: i64, commits: &[Commit]) -> Result<usize, AppError> {
        let tx = self.conn.transaction()?;
        let added = insert_commits(&tx, repo_id, commits, "backfill")?;
        tx.commit()?;
        Ok(added)
    }

    /// Stores one repo's scan in a single transaction. Existing rows are never changed,
    /// except the log states, which replace the old ones.
    pub fn record(
        &mut self,
        repo_id: i64,
        entries: &[Entry],
        commits: &[Commit],
        states: &[LogState],
    ) -> Result<(), AppError> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO reflog_entries (repo_id, ref, hash, op, ts, message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for e in entries {
                stmt.execute(params![
                    repo_id,
                    e.ref_name,
                    e.hash,
                    e.op,
                    iso(e.ts),
                    e.message
                ])?;
            }
            insert_commits(&tx, repo_id, commits, "reflog")?;
            let mut state_stmt = tx.prepare(
                "INSERT INTO repo_state (repo_id, log_path, size, mtime) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (repo_id, log_path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime",
            )?;
            for s in states {
                state_stmt.execute(params![repo_id, s.path, s.size, s.mtime])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Size and mtime of every session file when it was last scanned.
    pub fn session_states(&self) -> Result<Vec<LogState>, AppError> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, size, mtime FROM session_state ORDER BY path")?;
        let rows = stmt.query_map([], |r| {
            Ok(LogState {
                path: r.get(0)?,
                size: r.get(1)?,
                mtime: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Stores one session file in a single transaction. The session row is upserted,
    /// keeping stored values the new read lacks; prompts are only ever added. Returns how
    /// many prompts were new.
    pub fn record_session(&mut self, parsed: &Parsed, state: &LogState) -> Result<usize, AppError> {
        let tx = self.conn.transaction()?;
        let mut added = 0usize;
        if let Some(s) = &parsed.session {
            tx.execute(
                "INSERT INTO sessions (id, cwd, git_branch, title, started, ended, model,
                     cost_usd, lines_added, lines_removed, cc_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT (id) DO UPDATE SET
                     title = COALESCE(excluded.title, title),
                     started = min(started, excluded.started),
                     ended = max(ended, excluded.ended),
                     model = COALESCE(excluded.model, model),
                     cost_usd = COALESCE(excluded.cost_usd, cost_usd),
                     lines_added = COALESCE(excluded.lines_added, lines_added),
                     lines_removed = COALESCE(excluded.lines_removed, lines_removed),
                     cc_version = COALESCE(excluded.cc_version, cc_version)",
                params![
                    s.id,
                    s.cwd,
                    s.git_branch,
                    s.title,
                    iso(s.started),
                    iso(s.ended),
                    s.model,
                    s.cost_usd,
                    s.lines_added,
                    s.lines_removed,
                    s.cc_version,
                ],
            )?;
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO prompts (uuid, session_id, ts, kind, text)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for p in &parsed.prompts {
                let n = stmt.execute(params![p.uuid, s.id, iso(p.ts), p.kind.as_str(), p.text])?;
                added = added.saturating_add(n);
            }
        }
        tx.execute(
            "INSERT INTO session_state (path, size, mtime) VALUES (?1, ?2, ?3)
             ON CONFLICT (path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime",
            params![state.path, state.size, state.mtime],
        )?;
        tx.commit()?;
        Ok(added)
    }

    /// Sessions that started in `[from, to)`, or that have a prompt in `[from, to)`, oldest
    /// start first. Only prompts in `[from, to)` are loaded; a session with none still shows.
    pub fn sessions_between(
        &self,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<SessionRow>, AppError> {
        let (from, to) = (iso(from), iso(to));
        self.sessions(
            "WHERE (started >= ?1 AND started < ?2)
                OR id IN (SELECT session_id FROM prompts WHERE ts >= ?1 AND ts < ?2)
             ORDER BY started",
            params![from, to],
            None,
            Some(from.as_str()),
            Some(to.as_str()),
        )
    }

    /// Sessions with a prompt at or after `since` holding every word of `query`, with only
    /// the matching prompts at or after `since`.
    pub fn search_sessions(
        &self,
        query: &str,
        since: Option<Timestamp>,
    ) -> Result<Vec<SessionRow>, AppError> {
        let fts = fts_query(query);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        let since = since.map(iso);
        self.sessions(
            "WHERE id IN (SELECT session_id FROM prompts WHERE rowid IN
                          (SELECT rowid FROM prompts_fts WHERE prompts_fts MATCH ?1)
                          AND (?2 IS NULL OR ts >= ?2))
             ORDER BY started",
            params![fts, since],
            Some(&fts),
            since.as_deref(),
            None,
        )
    }

    /// Every known repo path, sorted.
    pub fn repo_paths(&self) -> Result<Vec<String>, AppError> {
        let mut stmt = self.conn.prepare("SELECT path FROM repos ORDER BY path")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Runs the session query with `tail` and loads each session's prompts: all of them, or
    /// only those matching `fts` and/or falling in `[prompt_from, prompt_to)` (either bound
    /// may be absent).
    fn sessions(
        &self,
        tail: &str,
        params: impl rusqlite::Params,
        fts: Option<&str>,
        prompt_from: Option<&str>,
        prompt_to: Option<&str>,
    ) -> Result<Vec<SessionRow>, AppError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, cwd, git_branch, title, started, ended, model, cost_usd,
                    lines_added, lines_removed, cc_version
             FROM sessions {tail}"
        ))?;
        let mut prompts = self.conn.prepare(
            "SELECT uuid, ts, kind, text FROM prompts
             WHERE session_id = ?1
             AND (?2 IS NULL OR rowid IN (SELECT rowid FROM prompts_fts WHERE prompts_fts MATCH ?2))
             AND (?3 IS NULL OR ts >= ?3)
             AND (?4 IS NULL OR ts < ?4)
             ORDER BY ts, uuid",
        )?;
        let found = stmt.query_map(params, |r| {
            Ok(Session {
                id: r.get(0)?,
                cwd: r.get(1)?,
                git_branch: r.get(2)?,
                title: r.get(3)?,
                started: timestamp(r, 4)?,
                ended: timestamp(r, 5)?,
                model: r.get(6)?,
                cost_usd: r.get(7)?,
                lines_added: r.get(8)?,
                lines_removed: r.get(9)?,
                cc_version: r.get(10)?,
            })
        })?;
        let mut rows = Vec::new();
        for session in found {
            let session = session?;
            let prompts = prompts
                .query_map(params![session.id, fts, prompt_from, prompt_to], |r| {
                    let kind: String = r.get(2)?;
                    Ok(Prompt {
                        uuid: r.get(0)?,
                        ts: timestamp(r, 1)?,
                        kind: Kind::parse(&kind),
                        text: r.get(3)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            rows.push(SessionRow { session, prompts });
        }
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::{Kind, Parsed, Prompt, Session};
    use std::path::PathBuf;

    fn temp_db(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("trail-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("trail.db")
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn entry(hash: &str) -> Entry {
        Entry {
            ref_name: "refs/heads/main".into(),
            hash: hash.into(),
            op: "commit".into(),
            ts: ts("2026-09-25T10:00:00Z"),
            message: "subject".into(),
        }
    }

    fn commit(hash: &str) -> Commit {
        Commit {
            hash: hash.into(),
            author_name: "BK".into(),
            author_email: "bk@example.com".into(),
            author_date: ts("2026-09-25T09:59:00Z"),
            committer_date: ts("2026-09-25T10:00:00Z"),
            subject: "feat: retry policy".into(),
            body: "Backs off exponentially.".into(),
            files: vec![
                FileStat {
                    path: "src/retry.rs".into(),
                    insertions: Some(10),
                    deletions: Some(2),
                },
                FileStat {
                    path: "logo.png".into(),
                    insertions: None,
                    deletions: None,
                },
            ],
        }
    }

    fn count(store: &Store, table: &str) -> i64 {
        store
            .conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn open_is_idempotent_and_uses_wal() {
        let path = temp_db("open");
        drop(Store::open(&path).unwrap());
        let store = Store::open(&path).unwrap();
        let mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn repo_id_is_stable() {
        let store = Store::open(&temp_db("repo")).unwrap();
        let a = store.repo_id("C:/src/a").unwrap();
        assert_eq!(store.repo_id("C:/src/a").unwrap(), a);
        assert_ne!(store.repo_id("C:/src/b").unwrap(), a);
    }

    #[test]
    fn record_is_append_only_and_idempotent() {
        let mut store = Store::open(&temp_db("record")).unwrap();
        let repo = store.repo_id("C:/src/a").unwrap();
        let hashes = ["a".repeat(40), "b".repeat(40)];
        assert_eq!(store.missing_hashes(repo, &hashes).unwrap(), hashes);

        let state = LogState {
            path: "C:/src/a/.git/logs/HEAD".into(),
            size: 100,
            mtime: 1,
        };
        let entries = [entry(&hashes[0]), entry(&hashes[1])];
        let commits = [commit(&hashes[0])];
        store
            .record(repo, &entries, &commits, std::slice::from_ref(&state))
            .unwrap();
        assert_eq!(
            store.missing_hashes(repo, &hashes).unwrap(),
            [hashes[1].clone()]
        );

        // Same data again adds nothing; a changed log state replaces the old one.
        let grown = LogState {
            size: 200,
            mtime: 2,
            ..state
        };
        store
            .record(repo, &entries, &commits, std::slice::from_ref(&grown))
            .unwrap();
        assert_eq!(count(&store, "reflog_entries"), 2);
        assert_eq!(count(&store, "commits"), 1);
        assert_eq!(count(&store, "commit_files"), 2);
        assert_eq!(store.log_states(repo).unwrap(), [grown]);
    }

    #[test]
    fn timestamps_are_stored_as_utc_iso() {
        let mut store = Store::open(&temp_db("ts")).unwrap();
        let repo = store.repo_id("C:/src/a").unwrap();
        let mut c = commit(&"c".repeat(40));
        c.author_date = "2026-09-25T11:59:00+02:00[Europe/Oslo]"
            .parse::<jiff::Zoned>()
            .unwrap()
            .timestamp();
        store.record(repo, &[], &[c], &[]).unwrap();
        let stored: String = store
            .conn
            .query_row("SELECT author_date FROM commits", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, "2026-09-25T09:59:00Z");
    }

    #[test]
    fn commits_between_filters_by_date_and_repo() {
        let mut store = Store::open(&temp_db("between")).unwrap();
        let a = store.repo_id("C:/src/app").unwrap();
        let b = store.repo_id("C:/src/Other").unwrap();
        let (h1, h2) = ("1".repeat(40), "2".repeat(40));
        let head = Entry {
            ref_name: "HEAD".into(),
            ..entry(&h1)
        };
        store
            .record(a, &[head, entry(&h1)], &[commit(&h1)], &[])
            .unwrap();
        let mut late = commit(&h2);
        late.author_date = ts("2026-09-26T08:00:00Z");
        let detached = Entry {
            ref_name: "HEAD".into(),
            ..entry(&h2)
        };
        store.record(b, &[detached], &[late], &[]).unwrap();

        let day = store
            .commits_between(ts("2026-09-25T00:00:00Z"), ts("2026-09-26T00:00:00Z"), None)
            .unwrap();
        assert_eq!(day.len(), 1);
        assert_eq!(day[0].repo, "C:/src/app");
        assert_eq!(day[0].branch, "main");
        assert_eq!(day[0].commit.files, commit(&h1).files);

        let other = store
            .commits_between(
                ts("2026-09-01T00:00:00Z"),
                ts("2026-10-01T00:00:00Z"),
                Some("other"),
            )
            .unwrap();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].commit.hash, h2);
        assert_eq!(other[0].branch, "HEAD");
    }

    #[test]
    fn backfill_adds_only_unknown_commits_with_their_source() {
        let mut store = Store::open(&temp_db("backfill")).unwrap();
        let repo = store.repo_id("C:/src/a").unwrap();
        let (h1, h2) = ("1".repeat(40), "2".repeat(40));
        store.record(repo, &[], &[commit(&h1)], &[]).unwrap();
        assert_eq!(
            store.backfill(repo, &[commit(&h1), commit(&h2)]).unwrap(),
            1
        );
        let sources: Vec<(String, String)> = store
            .conn
            .prepare("SELECT hash, source FROM commits ORDER BY hash")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            sources,
            [(h1, "reflog".to_owned()), (h2, "backfill".to_owned())]
        );
        assert_eq!(count(&store, "commit_files"), 4);
    }

    #[test]
    fn fts_query_quotes_each_word() {
        assert_eq!(fts_query("retry policy"), "\"retry\" \"policy\"");
        assert_eq!(fts_query("foo-bar \"x"), "\"foo-bar\" \"\"\"x\"");
        assert_eq!(fts_query("   "), "");
    }

    #[test]
    fn search_matches_all_words_with_filters() {
        let mut store = Store::open(&temp_db("search")).unwrap();
        let a = store.repo_id("C:/src/app").unwrap();
        let b = store.repo_id("C:/src/other").unwrap();
        let (h1, h2) = ("1".repeat(40), "2".repeat(40));
        store.record(a, &[entry(&h1)], &[commit(&h1)], &[]).unwrap();
        let mut old = commit(&h2);
        old.author_date = ts("2026-01-01T00:00:00Z");
        store.record(b, &[], &[old], &[]).unwrap();

        let hashes = |rows: Vec<Row>| rows.into_iter().map(|r| r.commit.hash).collect::<Vec<_>>();
        assert_eq!(
            hashes(store.search("retry src", None, None).unwrap()),
            [h2, h1.clone()]
        );
        assert!(
            store
                .search("retry nothing", None, None)
                .unwrap()
                .is_empty()
        );
        assert!(store.search("   ", None, None).unwrap().is_empty());
        assert_eq!(
            hashes(store.search("retry", Some("APP"), None).unwrap()),
            std::slice::from_ref(&h1)
        );
        let since = Some(ts("2026-09-01T00:00:00Z"));
        assert_eq!(hashes(store.search("retry", None, since).unwrap()), [h1]);
    }

    #[test]
    fn repo_summaries_count_commits() {
        let mut store = Store::open(&temp_db("summaries")).unwrap();
        let a = store.repo_id("C:/src/a").unwrap();
        store.repo_id("C:/src/empty").unwrap();
        store
            .record(
                a,
                &[],
                &[commit(&"1".repeat(40)), commit(&"2".repeat(40))],
                &[],
            )
            .unwrap();
        assert_eq!(
            store.repo_summaries().unwrap(),
            [
                RepoSummary {
                    path: "C:/src/a".into(),
                    commits: 2,
                    last: Some(ts("2026-09-25T09:59:00Z")),
                },
                RepoSummary {
                    path: "C:/src/empty".into(),
                    commits: 0,
                    last: None,
                },
            ]
        );
    }

    fn session(id: &str, started: &str) -> Session {
        Session {
            id: id.into(),
            cwd: r"C:\x\trail".into(),
            git_branch: "main".into(),
            title: Some("Title".into()),
            started: ts(started),
            ended: ts("2026-09-25T12:00:00Z"),
            model: Some("claude-opus-5-5".into()),
            cost_usd: Some(1.5),
            lines_added: Some(3),
            lines_removed: Some(1),
            cc_version: Some("2.1.282".into()),
        }
    }

    fn prompt(uuid: &str, at: &str, text: &str) -> Prompt {
        Prompt {
            uuid: uuid.into(),
            ts: ts(at),
            kind: Kind::Typed,
            text: text.into(),
        }
    }

    fn state(path: &str, size: i64) -> LogState {
        LogState {
            path: path.into(),
            size,
            mtime: 7,
        }
    }

    #[test]
    fn record_session_is_append_only_and_idempotent() {
        let mut store = Store::open(&temp_db("sessions")).unwrap();
        let parsed = Parsed {
            session: Some(session("s1", "2026-09-25T10:00:00Z")),
            prompts: vec![
                prompt("u1", "2026-09-25T10:00:00Z", "retry policy please"),
                Prompt {
                    kind: Kind::Answer,
                    ..prompt("u2", "2026-09-25T10:05:00Z", "Q: A?\nA: yes")
                },
            ],
        };
        assert_eq!(store.record_session(&parsed, &state("f1", 10)).unwrap(), 2);
        assert_eq!(store.record_session(&parsed, &state("f1", 10)).unwrap(), 0);
        assert_eq!(store.session_states().unwrap(), vec![state("f1", 10)]);

        // A later read of a file without title or cost keeps what was stored, and an
        // earlier start moves `started` back but a later one never moves it forward.
        let bare = Parsed {
            session: Some(Session {
                title: None,
                cost_usd: None,
                model: None,
                started: ts("2026-09-25T11:00:00Z"),
                ended: ts("2026-09-25T13:00:00Z"),
                ..session("s1", "2026-09-25T11:00:00Z")
            }),
            prompts: vec![prompt("u3", "2026-09-25T13:00:00Z", "more")],
        };
        assert_eq!(store.record_session(&bare, &state("f1", 20)).unwrap(), 1);
        assert_eq!(store.session_states().unwrap(), vec![state("f1", 20)]);

        let rows = store
            .sessions_between(ts("2026-09-25T00:00:00Z"), ts("2026-09-26T00:00:00Z"))
            .unwrap();
        assert_eq!(rows.len(), 1);
        let row = rows.first().unwrap();
        assert_eq!(row.session.title.as_deref(), Some("Title"));
        assert_eq!(row.session.cost_usd, Some(1.5));
        assert_eq!(row.session.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(row.session.started, ts("2026-09-25T10:00:00Z"));
        assert_eq!(row.session.ended, ts("2026-09-25T13:00:00Z"));
        let uuids: Vec<_> = row.prompts.iter().map(|p| p.uuid.as_str()).collect();
        assert_eq!(uuids, ["u1", "u2", "u3"]);
        assert_eq!(row.prompts.get(1).unwrap().kind, Kind::Answer);
    }

    #[test]
    fn file_without_session_only_records_its_state() {
        let mut store = Store::open(&temp_db("nosession")).unwrap();
        assert_eq!(
            store
                .record_session(&Parsed::default(), &state("empty", 0))
                .unwrap(),
            0
        );
        assert_eq!(store.session_states().unwrap(), vec![state("empty", 0)]);
    }

    #[test]
    fn sessions_between_uses_start_time() {
        let mut store = Store::open(&temp_db("between")).unwrap();
        for (id, start) in [
            ("early", "2026-09-24T23:00:00Z"),
            ("in", "2026-09-25T09:00:00Z"),
        ] {
            let parsed = Parsed {
                session: Some(session(id, start)),
                prompts: Vec::new(),
            };
            store.record_session(&parsed, &state(id, 1)).unwrap();
        }
        let rows = store
            .sessions_between(ts("2026-09-25T00:00:00Z"), ts("2026-09-26T00:00:00Z"))
            .unwrap();
        let ids: Vec<_> = rows.iter().map(|r| r.session.id.as_str()).collect();
        assert_eq!(ids, ["in"]);
    }

    #[test]
    fn sessions_between_includes_sessions_with_prompts_in_range() {
        let mut store = Store::open(&temp_db("spanning")).unwrap();
        let parsed = Parsed {
            session: Some(Session {
                ended: ts("2026-09-25T09:00:00Z"),
                ..session("s1", "2026-09-24T23:00:00Z")
            }),
            prompts: vec![
                prompt("u1", "2026-09-24T23:30:00Z", "day one"),
                prompt("u2", "2026-09-25T08:00:00Z", "day two"),
            ],
        };
        store.record_session(&parsed, &state("f", 1)).unwrap();

        let day1 = store
            .sessions_between(ts("2026-09-24T00:00:00Z"), ts("2026-09-25T00:00:00Z"))
            .unwrap();
        assert_eq!(day1.len(), 1);
        let uuids: Vec<_> = day1[0].prompts.iter().map(|p| p.uuid.as_str()).collect();
        assert_eq!(uuids, ["u1"]);

        let day2 = store
            .sessions_between(ts("2026-09-25T00:00:00Z"), ts("2026-09-26T00:00:00Z"))
            .unwrap();
        assert_eq!(day2.len(), 1);
        let uuids: Vec<_> = day2[0].prompts.iter().map(|p| p.uuid.as_str()).collect();
        assert_eq!(uuids, ["u2"]);
    }

    #[test]
    fn sessions_between_keeps_a_session_whose_prompts_are_all_out_of_range() {
        let mut store = Store::open(&temp_db("outofrange")).unwrap();
        let parsed = Parsed {
            session: Some(session("s1", "2026-09-25T10:00:00Z")),
            prompts: vec![prompt("u1", "2026-09-26T00:30:00Z", "next day")],
        };
        store.record_session(&parsed, &state("f", 1)).unwrap();
        let rows = store
            .sessions_between(ts("2026-09-25T00:00:00Z"), ts("2026-09-26T00:00:00Z"))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].prompts.is_empty());
    }

    #[test]
    fn search_sessions_filters_prompts_by_since_within_a_session() {
        let mut store = Store::open(&temp_db("searchsince")).unwrap();
        let parsed = Parsed {
            session: Some(Session {
                ended: ts("2026-09-27T00:00:00Z"),
                ..session("s1", "2026-09-25T10:00:00Z")
            }),
            prompts: vec![
                prompt("u1", "2026-09-25T10:00:00Z", "retry policy early"),
                prompt("u2", "2026-09-26T10:00:00Z", "retry policy late"),
            ],
        };
        store.record_session(&parsed, &state("f", 1)).unwrap();
        let rows = store
            .search_sessions("retry", Some(ts("2026-09-26T00:00:00Z")))
            .unwrap();
        assert_eq!(rows.len(), 1);
        let uuids: Vec<_> = rows[0].prompts.iter().map(|p| p.uuid.as_str()).collect();
        assert_eq!(uuids, ["u2"]);
    }

    #[test]
    fn search_sessions_returns_only_matching_prompts() {
        let mut store = Store::open(&temp_db("searchs")).unwrap();
        let parsed = Parsed {
            session: Some(session("s1", "2026-09-25T10:00:00Z")),
            prompts: vec![
                prompt("u1", "2026-09-25T10:00:00Z", "add a retry policy"),
                prompt("u2", "2026-09-25T10:01:00Z", "unrelated"),
            ],
        };
        store.record_session(&parsed, &state("f", 1)).unwrap();
        let rows = store.search_sessions("retry", None).unwrap();
        assert_eq!(rows.len(), 1);
        let uuids: Vec<_> = rows
            .first()
            .unwrap()
            .prompts
            .iter()
            .map(|p| p.uuid.as_str())
            .collect();
        assert_eq!(uuids, ["u1"]);
        assert!(store.search_sessions("nothing", None).unwrap().is_empty());
        assert!(store.search_sessions("   ", None).unwrap().is_empty());
        assert!(
            store
                .search_sessions("retry", Some(ts("2026-09-26T00:00:00Z")))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn repo_paths_lists_known_repos() {
        let store = Store::open(&temp_db("paths")).unwrap();
        store.repo_id("C:/b").unwrap();
        store.repo_id("C:/a").unwrap();
        assert_eq!(store.repo_paths().unwrap(), ["C:/a", "C:/b"]);
    }

    #[test]
    fn fts_covers_subject_body_and_paths() {
        let mut store = Store::open(&temp_db("fts")).unwrap();
        let repo = store.repo_id("C:/src/a").unwrap();
        store
            .record(repo, &[], &[commit(&"d".repeat(40))], &[])
            .unwrap();
        for query in ["retry", "exponentially", "logo"] {
            let hits: i64 = store
                .conn
                .query_row(
                    "SELECT count(*) FROM commits_fts WHERE commits_fts MATCH ?1",
                    [query],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(hits, 1, "{query}");
        }
    }
}
