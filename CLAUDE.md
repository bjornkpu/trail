# trail

Rust CLI that records every local commit by scanning git reflogs, and every Claude Code prompt
from session transcripts, into SQLite. User-facing behaviour and the reasoning behind it live in
`README.md`.

## Commands

- `cargo nextest run`: tests (use this, not `cargo test`)
- `cargo clippy --all-targets -- -D warnings`: must be clean; lints are `deny`, so this is the
  compile gate
- `cargo fmt --check`: formatting
- `cargo deny check`, `cargo machete`: dependency advisories, licenses, bans, unused crates
- `bacon clippy-all` / `bacon nextest`: watch mode
- `cargo run -- <args>`: run the CLI; set `TRAIL_HOME` to a temp dir to keep your real DB clean
- `bd ready`: what is buildable next

The gate: fmt, clippy, nextest, all green before claiming anything works. The Stop hook in
`.claude/settings.json` runs it at the end of every turn and blocks while it is red. CI runs the
same gate plus deny and machete, with tests on Windows and Ubuntu.

## Workflow

1. Brainstorm the feature (superpowers), then a spec and plan under `docs/superpowers/`.
2. Track the work in beads (`bd`), not in docs.
3. Build it with TDD, one behaviour at a time: red, green, refactor.
4. When a design settles, fold the decisions into the tracked docs: `README.md` for behaviour
   and the reasoning behind it, `docs/invariants.md` for rules with the tests that pin them,
   and `CLAUDE.md` for the module map and the repo-specific facts below.

Specs, plans and beads are local only. They are gitignored and never pushed (never
`bd dolt push`). Anything that must outlive the machine goes into the three tracked docs.

## Decided, do not ask

Everything in `docs/conventions/` is BK's standing preference: crates, layout, architecture,
testing, errors, style. Brainstorming, grilling and planning sessions treat it as decided. Ask
only about features, the domain, and real conflicts between a feature and a convention; when
you raise a conflict, name the convention.

## Hard rules

- Never weaken `[lints]` in `Cargo.toml` to make code compile. No `unwrap`/`expect`/`panic`/
  `todo`/indexing/`as` casts in non-test code; `clippy.toml` allows them in tests.
  `#[allow(clippy::...)]` goes on one item only, with a one-line comment saying why. Ask BK
  before relaxing `arithmetic_side_effects` or `as_conversions`.
- No `unsafe` (`unsafe_code = "forbid"`).
- Pure core, thin IO shell: only `src/io/` and `src/main.rs` do IO.
- Never break an invariant in `docs/invariants.md`. An invariant without a test is a bug.
- Never accept a snapshot you have not read.
- No crate outside `docs/conventions/crates.md` without asking BK. Never an alternative for a
  job a listed crate does. `gix` only if spawning `git` becomes a measured problem; never
  `git2`.
- No test needs network, a GPU, or the user's real config.

## Commits

Conventional Commits. release-plz builds `CHANGELOG.md` from the subjects, so a subject
describes the change for a user: never a bead id, never "this commit".

## Map

```
src/
  main.rs           clap commands, logging init, scan loop; anyhow only here  [wiring]
  error.rs          AppError (thiserror)
  domain.rs         pure: facts in, plan out                                  [pure]
    paths.rs        XDG + TRAIL_HOME resolution, Claude config dir
    reflog.rs       parse reflog line, commit-op filter
    claude.rs       session JSONL -> session + prompts, cwd -> repo link
    view.rs         rebase dedupe, grouping, text and JSON rendering
    range.rs        RANGE argument -> local date span
  io.rs             everything that touches the outside world                 [IO]
    config.rs       optional config.toml, defaults when absent
    discover.rs     walk roots -> repos (git dirs + worktrees)
    git.rs          git show --numstat for unseen hashes, git log for backfill
    store.rs        rusqlite schema, inserts, queries, FTS5
    schedule.rs     install / uninstall of the scheduled scan
tests/
  scan.rs           the binary end to end, TRAIL_HOME and CLAUDE_CONFIG_DIR in a temp dir
```

Module boundaries may shift; the pure/IO split does not.

## Git access

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
  with real reflog lines (INV-4).

## Claude sessions

- `<CLAUDE_CONFIG_DIR or ~/.claude>/projects/*/*.jsonl`, one file per session. Subagent files
  one level deeper are never read (INV-5). The format is internal: parse `serde_json::Value`,
  skip what does not fit.
- Kept: `user` lines with `origin.kind == "human"` and string content (typed), and `user` lines
  whose `toolUseResult.answers` is an object (AskUserQuestion answers). Title from the last
  `ai-title`, cost from the last `cost-state`.
- Same size+mtime skip as reflogs, in `session_state`. Sessions are linked to repos at query
  time, never stored.

## Storage

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
warning and continue; the reflog entry is still stored. Database failures are fatal. Logging
rules are in `docs/conventions/errors.md`.

## Conventions

Read the one that matches what you are about to do:

- `docs/conventions/architecture.md`: before adding a module, a trait, or anything with IO.
- `docs/conventions/testing.md`: before writing or changing a test.
- `docs/conventions/errors.md`: before adding an error variant, a log line, or output.
- `docs/conventions/style.md`: before writing code; the lint cheat sheet is there.
- `docs/conventions/crates.md`: before adding a dependency.
- `docs/releasing.md`: before touching versions, tags, or release config.
