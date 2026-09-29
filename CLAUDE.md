# trail

Rust CLI that records every local commit by scanning git reflogs, and every Claude Code prompt
from session transcripts, into SQLite. User-facing behaviour and the reasoning behind it live in
`README.md`. Work is tracked in beads (`bd ready`), not in docs.

## Commands

- `cargo nextest run`: tests (use this, not `cargo test`)
- `cargo clippy --all-targets -- -D warnings`: must be clean; lints are `deny`, so this is the compile gate
- `cargo fmt --check`: formatting
- `bacon clippy` / `bacon nextest`: watch mode
- `cargo run -- <args>`: run the CLI; set `TRAIL_HOME` to a temp dir to keep your real DB clean

Before claiming anything works: clippy, fmt, nextest, all green.

## Commits

Conventional Commits. Subjects describe the change for a user, never a bead id.

## Lints

`[lints.clippy]` in `Cargo.toml` is pedantic + nursery + panic-denying lints. Never weaken it to
make code compile. No `unwrap`/`expect`/`panic`/`todo`/indexing/`as` casts in non-test code.
`clippy.toml` allows them in tests. `#[allow(clippy::...)]` needs a one-line comment saying why,
on one item only. Ask BK before relaxing `arithmetic_side_effects` or `as_conversions`.

`[lints.rust]` sets `unsafe_code = "forbid"`.

The Stop hook in `.claude/settings.json` runs fmt, clippy and nextest at the end of every turn
and blocks while they are red. `cargo deny check` and `cargo machete` check dependencies.

## Dependencies

Planned: `clap`, `anyhow`, `thiserror`, `rusqlite` (bundled), `jiff`, `serde`, `serde_json`,
`toml`, `tracing`, `tracing-appender`, `tracing-subscriber`; dev: `insta`. Add one when a task
needs it. Ask before adding anything else. `gix` only if spawning `git` becomes a measured
problem; never `git2`.

## Style

Same as lazydatabricks: edition 2024 idioms (`let ... else`, let chains, `?`, iterators),
`thiserror` `AppError` in `src/error.rs`, `anyhow` only in `main`, `#[must_use]` on pure
functions, `&str`/slices in parameters. Ponytail rule: smallest working change, no traits with
one implementation, no config for constants. Every non-trivial branch or parser leaves a test.

## Architecture

Pure core, thin IO shell. Logic lives in pure modules that take and return plain data.

```
src/
  main.rs      clap commands, anyhow at the boundary
  error.rs     AppError
  paths.rs     XDG + TRAIL_HOME resolution                          [pure]
  config.rs    optional config.toml, defaults when absent
  discover.rs  walk roots -> repos (git dirs + worktrees)            [IO]
  reflog.rs    parse reflog line, commit-op filter                   [pure]
  claude.rs    session JSONL -> session + prompts, cwd -> repo link  [pure]
  git.rs       git show --numstat for unseen hashes                  [IO]
  store.rs     rusqlite schema, inserts, queries, FTS5               [IO]
  view.rs      rebase dedupe, grouping, text and JSON rendering      [pure]
  range.rs     RANGE argument -> local date span                     [pure]
  schedule.rs  install / uninstall of the scheduled scan             [IO]
```

### Git access

- Reflog files are parsed directly: `<gitdir>/logs/HEAD`, `<common-dir>/logs/refs/heads/**`,
  `<common-dir>/worktrees/*/logs/HEAD`. Line format:
  `<old> <new> <name> <<email>> <unix-ts> <tz>\t<op>: <message>`.
- `git show --numstat` runs only for hashes not yet stored, one process per commit.
- A repo whose reflog files all match the size and mtime in `repo_state` is skipped without
  spawning anything.
- Kept ops: `commit` (plain, initial, amend, merge, cherry-pick), `cherry-pick`, `revert`, any
  rebase flavour (`rebase`, `rebase -i`, `pull --rebase`) with
  pick/reword/squash/fixup/edit/continue/merge, and `merge`/`pull`
  entries whose message starts with `Merge made by`. Everything else is dropped. Tests pin this
  with real reflog lines.

### Claude sessions

- `<CLAUDE_CONFIG_DIR or ~/.claude>/projects/*/*.jsonl`, one file per session. Subagent files
  one level deeper are never read. The format is internal: parse `serde_json::Value`, skip
  what does not fit.
- Kept: `user` lines with `origin.kind == "human"` and string content (typed), and `user` lines
  whose `toolUseResult.answers` is an object (AskUserQuestion answers). Title from the last
  `ai-title`, cost from the last `cost-state`.
- Same size+mtime skip as reflogs, in `session_state`. Sessions are linked to repos at query
  time, never stored.

### Storage

SQLite, WAL, busy timeout (scheduled scan and interactive commands overlap). Append-only: never
delete on reflog expiry or missing repo. Timestamps UTC ISO 8601, shown in local time via `jiff`.

```sql
repos          (id, path UNIQUE, first_seen)
repo_state     (repo_id, log_path, size, mtime, PK (repo_id, log_path))
reflog_entries (repo_id, ref, hash, op, ts, message, UNIQUE (repo_id, ref, hash, ts, op))
commits        (repo_id, hash, author_name, author_email, author_date, committer_date,
                subject, body, source, PK (repo_id, hash))
commit_files   (repo_id, hash, path, insertions, deletions)
commits_fts    FTS5 (subject, body, paths), synced by triggers
sessions       (id PK, cwd, git_branch, title, started, ended, model, cost_usd,
                lines_added, lines_removed, cc_version)
prompts        (uuid PK, session_id, ts, kind, text)
session_state  (path PK, size, mtime)
prompts_fts    FTS5 (text), synced by trigger
```

Scan is one transaction per repo. Per-repo failures (missing object, git not on PATH) log a
warning and continue; the reflog entry is still stored. Database failures are fatal.

### Logging

`tracing` to `trail.log` in the state dir via `tracing-appender`. Never stdout under `--quiet`.

### Testing

1. Unit tests on pure modules: reflog parser, op filter, dedupe, range parsing, paths.
2. `insta` snapshots of text output. Never accept a snapshot you haven't read.
3. One integration test with `TRAIL_HOME` in a temp dir and real `git`: commit, amend, branch,
   rebase, squash merge, delete branch. Every squashed-away commit must be stored, and a second
   scan adds nothing. A second integration test covers Claude sessions via CLAUDE_CONFIG_DIR.
