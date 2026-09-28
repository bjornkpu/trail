# trail

A local record of every commit you make on your machine, including the local commits that squash
merging erases from remote history, and of every prompt you give Claude Code. Look back on them by
day, search them, or pipe them into an LLM.

```
trail show week
trail search "retry policy"
trail show last-week --json | claude -p "Summarise what I delivered"
```

## Why a reflog scanner

Remote history loses your work once a branch is squash merged. A global `core.hooksPath`
post-commit hook seems like the fix, but it overrides every repo's own `.git/hooks`, loses to
repos that set a local `core.hooksPath` (husky), makes the `pre-commit` framework refuse to
install, and only sees commits made after it was installed.

Git already records everything you do locally in the reflog. trail reads the reflogs of every
repo under your roots, keeps the commit-creating operations (commit, amend, merge commits,
rebase picks, cherry-pick, revert), and copies each commit's details into its own SQLite
database. Reflog entries expire after 30 to 90 days; the copy does not. No git config is
touched and no author email list is needed: everything in a reflog was done on this machine.

Limits: a repo deleted before the next scan loses its unscanned commits, and repos outside the
configured roots are not seen.

Commits older than a repo's reflog (expired entries, a fresh clone) can be added once with
`trail backfill --author <email>`, repeated for each address you commit with. It reads `git log
--all`, so it only finds commits still reachable from a ref.

## Why Claude Code sessions

Most work now starts as a prompt. Claude Code keeps each session as a JSONL file under
`~/.claude/projects` (or `CLAUDE_CONFIG_DIR`) and deletes it after `cleanupPeriodDays`, 30 days
by default. trail copies what you wrote before that happens: your typed prompts, your answers
to Claude's multiple-choice questions, and each session's title, working directory, branch,
start and end, model and cost. Claude's own replies, tool output and subagent transcripts are
not stored. Sessions are shown under the repo their working directory is in.

To keep the transcripts themselves longer, set `"cleanupPeriodDays": 365` in
`~/.claude/settings.json`.

## Install

Windows, in PowerShell:

```
irm https://github.com/bjornkpu/trail/releases/latest/download/trail-installer.ps1 | iex
```

macOS and Linux:

```
curl -LsSf https://github.com/bjornkpu/trail/releases/latest/download/trail-installer.sh | sh
```

Both drop the binary in `~/.local/bin` and add it to your `PATH`. With a Rust toolchain,
`cargo install --git https://github.com/bjornkpu/trail` builds it from source. Archives for
every platform are on the [releases page](https://github.com/bjornkpu/trail/releases). The
crate is not on crates.io; the name `trail` is taken there.

The installers also drop `trail-update` next to `trail`. Run it to upgrade to the latest release
without the install script.

Then register the scheduled scan:

```
trail install
```

`trail install` registers a scheduled scan (Windows Task Scheduler: at logon and every 30
minutes, headless). On other systems it prints a crontab line. `trail uninstall` removes it.

## Usage

```
trail scan [--quiet]
trail show [RANGE] [--repo X] [--prompts] [--json] [--no-scan]
trail search <QUERY> [--repo X] [--since DATE] [--json] [--no-scan]
trail repos
trail backfill --author EMAIL [--author EMAIL...]
```

`RANGE` is `today` (default), `yesterday`, `week`, `last-week`, `2026-09-25`, `2026-W39` or
`2026-09-01..2026-09-25`. Weeks are ISO weeks, Monday to Sunday. `show` and `search` scan first
unless `--no-scan`.

`--prompts` lists each Claude session's prompts under it; `search` always lists the matching ones.

Commits are placed on the day of their author date. A rebased or amended commit keeps its author
date, so it shows once, as its newest version. All versions stay stored.

## Files

| What   | Default                          | Override          |
|--------|----------------------------------|-------------------|
| Config | `~/.config/trail/config.toml`    | `XDG_CONFIG_HOME` |
| DB     | `~/.local/share/trail/trail.db`  | `XDG_DATA_HOME`   |
| Log    | `~/.local/state/trail/trail.log` | `XDG_STATE_HOME`  |

`TRAIL_HOME` puts all three in one directory. The config is optional; the defaults are:

```toml
roots = ["~"]
max_depth = 4
skip = ["AppData", ".cache", "target", "node_modules"]
```

## Ad-hoc analysis with DuckDB

The database is plain SQLite, so the DuckDB CLI can query it directly:

```sql
ATTACH '~/.local/share/trail/trail.db' AS t (TYPE sqlite);
SELECT r.path, count(*) FROM t.commits c JOIN t.repos r ON r.id = c.repo_id
GROUP BY ALL ORDER BY 2 DESC;
```
