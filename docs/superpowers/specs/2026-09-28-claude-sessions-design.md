# Claude Code sessions as a trail source

Date: 2026-09-28. Status: approved in brainstorming, awaiting spec review.

## Goal

trail becomes an audit log of what BK does, built from evidence rather than memory. Commits are
the first source. Claude Code prompts are the second: most of BK's work is writing code and
prompting Claude, and the prompts record intent and decisions that commits do not. Later these
records feed synthesis of project descriptions and BK's role ("what did I actually do on X").
Synthesis, meetings and other sources are out of scope here; this work only has to capture
enough for them.

Claude Code deletes session transcripts after `cleanupPeriodDays` (default 30). That is the same
problem trail already solves for reflogs: copy before expiry, never delete.

## Source format (observed, Claude Code 2.1.2xx)

The format is internal and undocumented. Keys already vary between lines, so the parser must
tolerate drift.

- Sessions live in `$CLAUDE_CONFIG_DIR/projects/<slug>/<sessionId>.jsonl`, where
  `$CLAUDE_CONFIG_DIR` defaults to `~/.claude`. The slug is the cwd with separators replaced by
  `-`; its case varies (`c--Users` and `C--Users` both occur).
- Subagent transcripts live in `projects/<slug>/<sessionId>/subagents/agent-*.jsonl`. They are
  skipped: Claude wrote those prompts, not BK.
- One JSON object per line, with a `type`. Relevant types:
  - `user`: has `uuid`, `timestamp` (ISO 8601 UTC), `cwd`, `gitBranch`, `sessionId`, `version`,
    `message.content` (a string, or an array of blocks), and `origin.kind` on recent versions.
  - `assistant`: `message.model`, `message.content[]` with `tool_use` blocks (`id`, `name`,
    `input`).
  - `ai-title`: `aiTitle`. Repeats; the last one is current.
  - `cost-state`: `totalCostUSD`, `totalLinesAdded`, `totalLinesRemoved`. Repeats and grows; the
    last one is current.
- `~/.claude/history.jsonl` (prompt index) is not used: session files hold the same prompts with
  more context.

## What is kept

A **typed prompt** is a `user` line with `origin.kind == "human"` and string `message.content`.
Dropped: `origin.kind` of `peer` or `task-notification`, lines without `origin` (skill
expansions, older versions), `isMeta` lines, and `tool_result` content, except answers below.

An **answer** is BK's reply to an `AskUserQuestion` tool call. The `user` line carrying its
`tool_result` also has a `toolUseResult` object with `questions` (array of `{question, ...}`),
`answers` (question text to answer text) and, on most versions, `annotations` (question text to
`{notes?, preview?}`). Any `user` line whose `toolUseResult.answers` is an object becomes a
prompt of kind `answer`; no pairing with the `tool_use` is needed. Text is one block per
question in `questions` order: `Q: <question>\nA: <answer>`, plus `\nNotes: <notes>` when
annotations hold notes; blocks are joined by a blank line. The answer prompt takes the `uuid`
and `timestamp` of that `user` line.

Assistant text, thinking and other tool calls are not stored.

## Data model

Added to `SCHEMA` in `store.rs` with `CREATE ... IF NOT EXISTS`, like the existing tables.

```sql
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,          -- sessionId
    cwd TEXT NOT NULL,            -- first cwd seen in the file
    git_branch TEXT NOT NULL,     -- first gitBranch seen, '' when absent
    title TEXT,                   -- last ai-title
    started TEXT NOT NULL,        -- min timestamp, UTC ISO 8601
    ended TEXT NOT NULL,          -- max timestamp
    model TEXT,                   -- most frequent assistant message.model, ignoring <synthetic>
    cost_usd REAL,                -- last cost-state
    lines_added INTEGER,
    lines_removed INTEGER,
    cc_version TEXT               -- last version seen
);
CREATE TABLE IF NOT EXISTS prompts (
    uuid TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    ts TEXT NOT NULL,
    kind TEXT NOT NULL,           -- typed | answer
    text TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS prompts_session ON prompts (session_id, ts);
CREATE TABLE IF NOT EXISTS session_state (
    path TEXT PRIMARY KEY,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS prompts_fts USING fts5 (text);
-- trigger on prompts insert, same pattern as commits_fts
```

`sessions` rows are upserted, because title, cost and `ended` change while a session is live.
`prompts` are insert-or-ignore by `uuid`: append-only. Nothing is deleted when a session file
disappears.

A session has no stored repo link. It is linked at query time (see Linking), so a session
scanned before its repo was discovered links correctly later.

## Modules

- `src/claude.rs` [pure]: `parse(&str) -> Parsed { session: Option<Session>, prompts:
  Vec<Prompt> }`, working line by line on `serde_json::Value`. A line that is not valid JSON or
  lacks expected fields is skipped. `session` is `None` when the file has no `user` or
  `assistant` line with a timestamp. Also `link(cwd, repo_paths) -> Option<&str>`.
- `src/main.rs`: `scan()` scans sessions after repos.
- `src/store.rs`: tables, `session_states`, `record_session`, queries for sessions and prompts
  in a range and by FTS.
- `src/view.rs`: an item enum (commit or session) for text and JSON rendering.
- `src/paths.rs`: resolves the Claude dir from `CLAUDE_CONFIG_DIR` or `home/.claude`. No trail
  config key.

## Scan flow

After the repo scan, list `<claude dir>/projects/*/*.jsonl` (one level; `subagents/` is never
reached). For each file:

1. Stat it. If size and mtime match `session_state`, skip it.
2. Read the whole file and run `claude::parse`.
3. One transaction per file: upsert the session, insert-or-ignore the prompts, write
   `session_state`.

A missing Claude dir means no sessions and no error. A file that cannot be stat'ed or read logs
a warning and is skipped. Database errors are fatal, as today.

Live sessions are re-read whole on every scan (a few MB at most). Marked with a `ponytail:`
comment: switch to reading from a stored byte offset if scans get slow.

Scan output: `12 repos, 3 new commits, 2 sessions, 17 new prompts`, where sessions counts files
re-read.

## Linking

`link(cwd, repo_paths)` returns the longest repo path that equals `cwd` or is a path prefix of
it at a separator boundary. Comparison is ASCII case-insensitive and treats `/` and `\` as
equal. Worktrees are separate repos in `discover`, so they match directly. No match leaves the
session unlinked; it is grouped under its raw cwd.

`--repo X` keeps a session when its linked repo path, or its cwd when unlinked, contains `X`
(case-insensitive), matching the commit filter.

## Output

`trail show [RANGE]`: sessions are placed on the local day they started, under their linked
repo or raw cwd, in the same time-sorted list as commits. Branch column `claude`, subject is the
title (or the first line of the first prompt, cut to 80 chars, when there is no title), then
prompt count, duration (`2h05`) and cost (`$4.10`) when known.

```
Monday 2026-09-28
  C:\Users\BjornKristian.Punsvi\personal\trail
    09:12  main    feat: show commits once across clones   +40 -3
    13:10  claude  Brainstorm Claude sessions as source    6 prompts  2h05  $4.10
```

`--prompts` lists each session's prompts under its line: `      13:12  > <text>` for typed
prompts and `      13:20  ? <text>` for answers, with whitespace collapsed to single spaces and
cut to 100 characters plus `…`.

A session with no prompts in the range still shows when its start is in the range. An empty
result prints `Nothing recorded.` (was `No commits.`).

`trail search <QUERY>`: FTS runs over prompts as well as commits. A matching prompt shows its
session line with the matching prompts listed under it. `--since` and `--repo` apply to both.

`--json` (both commands): one array sorted by time, each item tagged with `type`. Commit items
are today's objects plus `"type": "commit"`. Session items:

```json
{"type": "session", "repo": "C:\\...\\trail", "cwd": "C:\\...\\trail", "id": "b45f2fc9-...",
 "title": "...", "started": "2026-09-28T13:10:00+02:00", "ended": "...", "model": "...",
 "cost_usd": 4.1, "lines_added": 0, "lines_removed": 0,
 "prompts": [{"ts": "...", "kind": "typed", "text": "full text"}]}
```

`repo` is `null` when unlinked. JSON always carries full prompt text (for `search`, only the
matching prompts); it is the feed for LLM synthesis.

## Testing

Unit, on pure code:

- `claude::parse` against real lines trimmed from BK's sessions and scrubbed of anything
  sensitive: typed prompt kept; `peer`, `task-notification`, skill expansion, `isMeta` and
  `tool_result` dropped; AskUserQuestion paired into an answer, with and without notes; last
  `ai-title` and last `cost-state` win; started/ended are min/max; a garbage line is skipped and
  the rest still parses; a file with no user or assistant lines gives no session.
- `link`: exact, subdirectory, longest prefix beats parent, case-insensitive, mixed separators,
  no match, and `trail` does not match `trail2`.
- `view`: insta snapshots for interleaved text, `--prompts`, and tagged JSON. Read every
  snapshot before accepting it.

Integration, extending `tests/scan.rs`: `CLAUDE_CONFIG_DIR` points at a temp dir holding a
fixture JSONL whose cwd is the test repo. Assert the session and its prompts are stored; a
second scan adds nothing; appending one typed prompt line and rescanning adds exactly one
prompt; `show --json` interleaves the session with the commits.

## Docs

- README: tagline covers commits and Claude Code prompts; a "Why Claude sessions" paragraph
  (30-day cleanup, `cleanupPeriodDays` tip, what is kept and skipped); `--prompts` in usage.
- CLAUDE.md: `claude.rs [pure]` in the architecture list, the new tables in the storage block,
  and a "Claude sessions" note next to "Git access".

## Out of scope

Synthesis, meetings and other sources, a unified `activity` view (add as a SQL `VIEW ... UNION
ALL` when a third source arrives), `history.jsonl`, subagent transcripts, assistant text, and
the per-model breakdown in `cost-state`.
