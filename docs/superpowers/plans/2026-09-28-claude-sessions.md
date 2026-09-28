# Claude Code Sessions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** trail scans Claude Code session transcripts and stores BK's typed prompts, AskUserQuestion answers and per-session metadata next to commits, and shows them interleaved in `show`, `search` and `--json`.

**Architecture:** New pure module `src/claude.rs` parses one session JSONL into a `Session` plus `Vec<Prompt>` and links a cwd to a known repo path. `store.rs` gains three tables and an FTS table, following the existing reflog pattern (size+mtime skip, one transaction per file, append-only). `view.rs` renders a time-sorted `Item` list (commit or session). `main.rs` wires the scan and the `--prompts` flag.

**Tech Stack:** Rust 2024, rusqlite (bundled, FTS5), serde_json, jiff, insta. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-28-claude-sessions-design.md`. Read it before any task.

## Global Constraints

- Before claiming a task is done: `cargo clippy --all-targets` clean, `cargo fmt --check` clean, `cargo nextest run` green. Use nextest, never `cargo test`. One exception: Tasks 1 to 3 add items that later tasks call, so rustc `dead_code` warnings for those items are expected until Task 4. Every other warning must be fixed. Never silence `dead_code` with an attribute.
- Lints are `deny`: no `unwrap`/`expect`/`panic`/`todo`/indexing/`as` casts/unchecked arithmetic in non-test code. Use `saturating_add`, `checked_*`, `.get()`, `try_from`. Never weaken `[lints.clippy]`. An `#[allow(clippy::...)]` needs a one-line comment saying why, on one item only.
- No new crates. `serde_json::Value` for parsing; no new `serde` structs for the input format.
- `#[must_use]` on pure functions. `&str`/slices in parameters. `thiserror` `AppError` in `src/error.rs`; `anyhow` only in `main`.
- Timestamps stored as UTC ISO 8601 via the existing `store::iso`; shown in local time.
- Append-only: never delete rows. `sessions` is upserted; `prompts` is insert-or-ignore.
- insta: never accept a snapshot you have not read line by line against the expected output in this plan.
- Tests never read the real `~/.claude`: every integration command sets `CLAUDE_CONFIG_DIR` to a temp dir.
- Conventional Commits; subjects describe the change for a user, never a bead id. No attribution lines.

## Review Focus

1. **Integration tests leaking into the real `~/.claude`.** Without `CLAUDE_CONFIG_DIR` in `Env::command`, every existing integration test would scan BK's 500 MB of sessions. Task 3 sets it for all commands and asserts `0 sessions` in the existing tests.
2. **A live session file with a half-written last line.** Claude appends while trail reads. The partial line must be skipped, not fail the file, and the next scan (size changed) picks it up. Task 1 pins "garbage line skipped"; Task 3 pins "append then rescan adds exactly one".
3. **The same `sessionId` appearing again with fewer fields** (a later file state without `ai-title` or `cost-state` lines, or a resumed session). The upsert must not wipe title/cost to NULL, and `started` must not move later. Task 2 pins this.
4. **`<synthetic>` model and sessions without `cost-state` or `ai-title`.** `model` ignores `<synthetic>`; missing title falls back to the first prompt; missing cost prints nothing. Tasks 1 and 4 pin this.
5. **Repo path prefix false positives and Windows case.** `C:\x\trail2` must not link to `C:\x\trail`; `c:\X\trail` must link to `C:\x\trail`. Task 1 pins this.

---

### Task 1: Pure session parser and repo linking (`src/claude.rs`)

**Files:**
- Create: `src/claude.rs`
- Modify: `src/main.rs:1` (add `mod claude;` in the alphabetical `mod` list, before `mod config;`)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces:
  - `pub struct Session { pub id: String, pub cwd: String, pub git_branch: String, pub title: Option<String>, pub started: Timestamp, pub ended: Timestamp, pub model: Option<String>, pub cost_usd: Option<f64>, pub lines_added: Option<i64>, pub lines_removed: Option<i64>, pub cc_version: Option<String> }` (derive `Debug, Clone, PartialEq`)
  - `pub enum Kind { Typed, Answer }` (derive `Debug, Clone, Copy, PartialEq, Eq`) with `pub const fn as_str(self) -> &'static str` (`"typed"`/`"answer"`) and `pub fn parse(s: &str) -> Self` (`"answer"` gives `Answer`, anything else `Typed`)
  - `pub struct Prompt { pub uuid: String, pub ts: Timestamp, pub kind: Kind, pub text: String }` (derive `Debug, Clone, PartialEq, Eq`)
  - `pub struct Parsed { pub session: Option<Session>, pub prompts: Vec<Prompt> }` (derive `Debug, Default`)
  - `pub fn parse(text: &str) -> Parsed`
  - `pub fn link<'a>(cwd: &str, repos: &'a [String]) -> Option<&'a str>`

- [ ] **Step 1: Write the failing tests**

Create `src/claude.rs` with only the test module below and `mod claude;` in `main.rs`. The lines are trimmed from real sessions (Claude Code 2.1.2xx).

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const TYPED: &str = r#"{"type":"user","uuid":"u1","timestamp":"2026-09-28T11:12:00.000Z","cwd":"C:\\x\\trail","gitBranch":"main","sessionId":"s1","version":"2.1.282","origin":{"kind":"human"},"promptSource":"typed","message":{"role":"user","content":"record claude sessions\nplease"}}"#;
    const PEER: &str = r#"{"type":"user","uuid":"u2","timestamp":"2026-09-28T11:13:00.000Z","cwd":"C:\\x\\trail","sessionId":"s1","origin":{"kind":"peer"},"message":{"role":"user","content":"from another session"}}"#;
    const NOTIFY: &str = r#"{"type":"user","uuid":"u3","timestamp":"2026-09-28T11:14:00.000Z","cwd":"C:\\x\\trail","sessionId":"s1","origin":{"kind":"task-notification"},"promptSource":"system","message":{"role":"user","content":"<task-notification>done</task-notification>"}}"#;
    const SKILL: &str = r#"{"type":"user","uuid":"u4","timestamp":"2026-09-28T11:15:00.000Z","cwd":"C:\\x\\trail","sessionId":"s1","message":{"role":"user","content":"<command-name>/caveman</command-name>"}}"#;
    const META: &str = r#"{"type":"user","uuid":"u5","timestamp":"2026-09-28T11:16:00.000Z","cwd":"C:\\x\\trail","sessionId":"s1","isMeta":true,"message":{"role":"user","content":[{"type":"text","text":"skill body"}]}}"#;
    const TOOL_RESULT: &str = r#"{"type":"user","uuid":"u6","timestamp":"2026-09-28T11:17:00.000Z","cwd":"C:\\x\\trail","sessionId":"s1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]},"toolUseResult":{"stdout":"ok"}}"#;
    const ANSWER: &str = r#"{"type":"user","uuid":"u7","timestamp":"2026-09-28T11:20:00.000Z","cwd":"C:\\x\\trail","sessionId":"s1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"Your questions have been answered"}]},"toolUseResult":{"questions":[{"question":"Capture?","header":"Capture","options":[],"multiSelect":false},{"question":"Subagents?","header":"Subagents","options":[],"multiSelect":false}],"answers":{"Subagents?":"Skip","Capture?":"Prompts + meta"},"annotations":{"Capture?":{"notes":"keep it small"},"Subagents?":{"preview":"ignored"}}}}"#;
    const ASSISTANT: &str = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-09-28T13:15:00.000Z","cwd":"C:\\x\\trail","sessionId":"s1","version":"2.1.290","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"hi"}]}}"#;
    const SYNTHETIC: &str = r#"{"type":"assistant","uuid":"a2","timestamp":"2026-09-28T12:00:00.000Z","sessionId":"s1","message":{"model":"<synthetic>","content":[]}}"#;
    const SYNTHETIC2: &str = r#"{"type":"assistant","uuid":"a3","timestamp":"2026-09-28T12:01:00.000Z","sessionId":"s1","message":{"model":"<synthetic>","content":[]}}"#;
    const TITLE1: &str = r#"{"type":"ai-title","aiTitle":"Old title","sessionId":"s1"}"#;
    const TITLE2: &str = r#"{"type":"ai-title","aiTitle":"Brainstorm sessions","sessionId":"s1"}"#;
    const COST1: &str = r#"{"type":"cost-state","sessionId":"s1","totalCostUSD":1.5,"totalLinesAdded":10,"totalLinesRemoved":1}"#;
    const COST2: &str = r#"{"type":"cost-state","sessionId":"s1","totalCostUSD":4.1,"totalLinesAdded":20,"totalLinesRemoved":3}"#;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn file(lines: &[&str]) -> String {
        lines.join("\n")
    }

    #[test]
    fn keeps_typed_prompts_and_drops_everything_else() {
        let parsed = parse(&file(&[TYPED, PEER, NOTIFY, SKILL, META, TOOL_RESULT, ASSISTANT]));
        assert_eq!(
            parsed.prompts,
            vec![Prompt {
                uuid: "u1".into(),
                ts: ts("2026-09-28T11:12:00Z"),
                kind: Kind::Typed,
                text: "record claude sessions\nplease".into(),
            }]
        );
    }

    #[test]
    fn answers_follow_question_order_with_notes() {
        let parsed = parse(&file(&[TYPED, ANSWER]));
        let answer = parsed.prompts.get(1).unwrap();
        assert_eq!(answer.kind, Kind::Answer);
        assert_eq!(answer.uuid, "u7");
        assert_eq!(answer.ts, ts("2026-09-28T11:20:00Z"));
        assert_eq!(
            answer.text,
            "Q: Capture?\nA: Prompts + meta\nNotes: keep it small\n\nQ: Subagents?\nA: Skip"
        );
    }

    #[test]
    fn answer_without_annotations() {
        let line = ANSWER.replace(
            r#","annotations":{"Capture?":{"notes":"keep it small"},"Subagents?":{"preview":"ignored"}}"#,
            "",
        );
        let parsed = parse(&line);
        assert_eq!(
            parsed.prompts.first().unwrap().text,
            "Q: Capture?\nA: Prompts + meta\n\nQ: Subagents?\nA: Skip"
        );
    }

    #[test]
    fn session_meta_takes_last_title_and_cost_and_min_max_times() {
        let parsed = parse(&file(&[
            TITLE1, COST1, TYPED, SYNTHETIC, SYNTHETIC2, ASSISTANT, TITLE2, COST2,
        ]));
        assert_eq!(
            parsed.session,
            Some(Session {
                id: "s1".into(),
                cwd: r"C:\x\trail".into(),
                git_branch: "main".into(),
                title: Some("Brainstorm sessions".into()),
                started: ts("2026-09-28T11:12:00Z"),
                ended: ts("2026-09-28T13:15:00Z"),
                model: Some("claude-opus-5-5".into()),
                cost_usd: Some(4.1),
                lines_added: Some(20),
                lines_removed: Some(3),
                cc_version: Some("2.1.290".into()),
            })
        );
    }

    #[test]
    fn garbage_and_truncated_lines_are_skipped() {
        let half = TYPED.get(..40).unwrap();
        let parsed = parse(&file(&["not json", TYPED, "", half]));
        assert_eq!(parsed.prompts.len(), 1);
        assert!(parsed.session.is_some());
    }

    #[test]
    fn no_user_or_assistant_lines_means_no_session() {
        let parsed = parse(&file(&[TITLE1, COST1]));
        assert_eq!(parsed.session, None);
        assert!(parsed.prompts.is_empty());
    }

    #[test]
    fn kind_round_trips() {
        for kind in [Kind::Typed, Kind::Answer] {
            assert_eq!(Kind::parse(kind.as_str()), kind);
        }
    }

    #[test]
    fn link_picks_the_longest_enclosing_repo() {
        let repos: Vec<String> = [r"C:\x", r"C:\x\trail", r"C:\x\trail2", "/home/bk/app"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(link(r"C:\x\trail", &repos), Some(r"C:\x\trail"));
        assert_eq!(link(r"C:\x\trail\src\deep", &repos), Some(r"C:\x\trail"));
        assert_eq!(link(r"c:\X\TRAIL", &repos), Some(r"C:\x\trail"));
        assert_eq!(link("C:/x/trail/src", &repos), Some(r"C:\x\trail"));
        assert_eq!(link(r"C:\x\trail2", &repos), Some(r"C:\x\trail2"));
        assert_eq!(link(r"C:\x\other", &repos), Some(r"C:\x"));
        assert_eq!(link(r"C:\xy", &repos), None);
        assert_eq!(link("/home/bk/app/", &repos), Some("/home/bk/app"));
        assert_eq!(link(r"D:\y", &repos), None);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run claude::`
Expected: compile errors (`parse`, `link`, `Session`, `Prompt`, `Kind` not found).

- [ ] **Step 3: Write the implementation**

Put this above the test module in `src/claude.rs`:

```rust
//! Claude Code session transcripts: one JSONL file per session under
//! `<claude dir>/projects/<slug>/`. The format is internal to Claude Code and drifts, so
//! every line is read as untyped JSON and anything unexpected is skipped.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde_json::Value;

/// One session's metadata. Title and cost are the last values the file holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub id: String,
    pub cwd: String,
    pub git_branch: String,
    pub title: Option<String>,
    pub started: Timestamp,
    pub ended: Timestamp,
    pub model: Option<String>,
    pub cost_usd: Option<f64>,
    pub lines_added: Option<i64>,
    pub lines_removed: Option<i64>,
    pub cc_version: Option<String>,
}

/// What BK wrote: a typed prompt, or answers to an `AskUserQuestion` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Typed,
    Answer,
}

impl Kind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Typed => "typed",
            Self::Answer => "answer",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Self {
        if s == "answer" { Self::Answer } else { Self::Typed }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub uuid: String,
    pub ts: Timestamp,
    pub kind: Kind,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct Parsed {
    /// `None` when no user or assistant line has a session id and a timestamp.
    pub session: Option<Session>,
    pub prompts: Vec<Prompt>,
}

/// Reads a session file. Lines that are not JSON (a half-written last line) are skipped.
#[must_use]
pub fn parse(text: &str) -> Parsed {
    let mut acc = Acc::default();
    for line in text.lines() {
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            acc.line(&value);
        }
    }
    acc.finish()
}

#[derive(Default)]
struct Acc {
    id: Option<String>,
    cwd: Option<String>,
    git_branch: Option<String>,
    title: Option<String>,
    started: Option<Timestamp>,
    ended: Option<Timestamp>,
    models: BTreeMap<String, usize>,
    cost_usd: Option<f64>,
    lines_added: Option<i64>,
    lines_removed: Option<i64>,
    cc_version: Option<String>,
    prompts: Vec<Prompt>,
}

fn str_at<'a>(v: &'a Value, pointer: &str) -> Option<&'a str> {
    v.pointer(pointer).and_then(Value::as_str)
}

impl Acc {
    fn line(&mut self, v: &Value) {
        match str_at(v, "/type") {
            Some("ai-title") => {
                if let Some(title) = str_at(v, "/aiTitle") {
                    self.title = Some(title.to_owned());
                }
            }
            Some("cost-state") => {
                if let Some(cost) = v.get("totalCostUSD").and_then(Value::as_f64) {
                    self.cost_usd = Some(cost);
                }
                if let Some(n) = v.get("totalLinesAdded").and_then(Value::as_i64) {
                    self.lines_added = Some(n);
                }
                if let Some(n) = v.get("totalLinesRemoved").and_then(Value::as_i64) {
                    self.lines_removed = Some(n);
                }
            }
            Some(kind @ ("user" | "assistant")) => {
                let Some(ts) = str_at(v, "/timestamp").and_then(|t| t.parse().ok()) else {
                    return;
                };
                let Some(id) = str_at(v, "/sessionId") else {
                    return;
                };
                self.id.get_or_insert_with(|| id.to_owned());
                if self.cwd.is_none() {
                    self.cwd = str_at(v, "/cwd").map(str::to_owned);
                }
                if self.git_branch.is_none() {
                    self.git_branch = str_at(v, "/gitBranch").map(str::to_owned);
                }
                if let Some(version) = str_at(v, "/version") {
                    self.cc_version = Some(version.to_owned());
                }
                self.started = Some(self.started.map_or(ts, |s| s.min(ts)));
                self.ended = Some(self.ended.map_or(ts, |e| e.max(ts)));
                if kind == "assistant" {
                    if let Some(model) = str_at(v, "/message/model")
                        && model != "<synthetic>"
                    {
                        let n = self.models.entry(model.to_owned()).or_default();
                        *n = n.saturating_add(1);
                    }
                } else {
                    self.user(v, ts);
                }
            }
            _ => {}
        }
    }

    fn user(&mut self, v: &Value, ts: Timestamp) {
        let Some(uuid) = str_at(v, "/uuid") else {
            return;
        };
        let (kind, text) = if str_at(v, "/origin/kind") == Some("human")
            && let Some(text) = str_at(v, "/message/content")
        {
            (Kind::Typed, text.to_owned())
        } else if let Some(text) = answers(v) {
            (Kind::Answer, text)
        } else {
            return;
        };
        self.prompts.push(Prompt {
            uuid: uuid.to_owned(),
            ts,
            kind,
            text,
        });
    }

    fn finish(self) -> Parsed {
        let session = match (self.id, self.started, self.ended) {
            (Some(id), Some(started), Some(ended)) => Some(Session {
                id,
                cwd: self.cwd.unwrap_or_default(),
                git_branch: self.git_branch.unwrap_or_default(),
                title: self.title,
                started,
                ended,
                model: self
                    .models
                    .into_iter()
                    .max_by_key(|(_, n)| *n)
                    .map(|(m, _)| m),
                cost_usd: self.cost_usd,
                lines_added: self.lines_added,
                lines_removed: self.lines_removed,
                cc_version: self.cc_version,
            }),
            _ => None,
        };
        Parsed {
            session,
            prompts: self.prompts,
        }
    }
}

/// `Q: ..\nA: ..[\nNotes: ..]` per question, in the order they were asked.
fn answers(v: &Value) -> Option<String> {
    let result = v.get("toolUseResult")?;
    let answers = result.get("answers")?.as_object()?;
    let blocks: Vec<String> = result
        .get("questions")?
        .as_array()?
        .iter()
        .filter_map(|q| {
            let question = q.get("question")?.as_str()?;
            let answer = answers.get(question)?.as_str()?;
            let notes = result
                .pointer("/annotations")
                .and_then(|a| a.get(question))
                .and_then(|a| a.get("notes"))
                .and_then(Value::as_str);
            Some(match notes {
                Some(notes) => format!("Q: {question}\nA: {answer}\nNotes: {notes}"),
                None => format!("Q: {question}\nA: {answer}"),
            })
        })
        .collect();
    (!blocks.is_empty()).then(|| blocks.join("\n\n"))
}

/// The longest repo path that is `cwd` or encloses it. ASCII case and `/` versus `\` are
/// ignored, since Windows reports the same directory both ways.
#[must_use]
pub fn link<'a>(cwd: &str, repos: &'a [String]) -> Option<&'a str> {
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_ascii_lowercase();
    let cwd = norm(cwd);
    repos
        .iter()
        .filter(|repo| {
            let repo = norm(repo);
            cwd.strip_prefix(&repo)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
        .max_by_key(|repo| repo.len())
        .map(String::as_str)
}
```

If clippy flags `option_if_let_else` or similar nursery lints, rewrite to the form clippy suggests; do not add `allow`.

- [ ] **Step 4: Run tests and gates**

Run: `cargo nextest run claude::` then `cargo clippy --all-targets` then `cargo fmt --check`
Expected: 8 tests pass. Only `dead_code` warnings remain (see Global Constraints).

- [ ] **Step 5: Commit**

```bash
git add src/claude.rs src/main.rs
git commit -m "feat: parse Claude Code session transcripts"
```

---

### Task 2: Store sessions and prompts (`src/store.rs`)

**Files:**
- Modify: `src/store.rs` (SCHEMA, new struct `SessionRow`, new `Store` methods, tests)

**Interfaces:**
- Consumes: `claude::{Session, Prompt, Kind, Parsed}` from Task 1. Existing `LogState`, `iso`, `timestamp`, `fts_query`.
- Produces:
  - `pub struct SessionRow { pub session: Session, pub prompts: Vec<Prompt> }` (derive `Debug`)
  - `Store::session_states(&self) -> Result<Vec<LogState>, AppError>`
  - `Store::record_session(&mut self, parsed: &Parsed, state: &LogState) -> Result<usize, AppError>` (returns new prompts)
  - `Store::sessions_between(&self, from: Timestamp, to: Timestamp) -> Result<Vec<SessionRow>, AppError>` (started in `[from, to)`, all prompts, oldest first)
  - `Store::search_sessions(&self, query: &str, since: Option<Timestamp>) -> Result<Vec<SessionRow>, AppError>` (sessions with a matching prompt, only matching prompts, `ended >= since`)
  - `Store::repo_paths(&self) -> Result<Vec<String>, AppError>`

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `src/store.rs` (it already has `temp_db` and `ts`):

```rust
    use crate::claude::{Kind, Parsed, Prompt, Session};

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
        for (id, start) in [("early", "2026-09-24T23:00:00Z"), ("in", "2026-09-25T09:00:00Z")] {
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run store::`
Expected: compile errors for the missing methods.

- [ ] **Step 3: Implement**

Append to `SCHEMA` (inside the string, after the last trigger):

```sql
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
```

Add near `Row`:

```rust
use crate::claude::{Kind, Parsed, Prompt, Session};

/// A stored session with the prompts a query asked for.
#[derive(Debug)]
pub struct SessionRow {
    pub session: Session,
    pub prompts: Vec<Prompt>,
}
```

Add to `impl Store`:

```rust
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

    /// Sessions that started in `[from, to)`, oldest first, with all their prompts.
    pub fn sessions_between(
        &self,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<SessionRow>, AppError> {
        self.sessions(
            "WHERE started >= ?1 AND started < ?2 ORDER BY started",
            params![iso(from), iso(to)],
            None,
        )
    }

    /// Sessions with a prompt holding every word of `query`, with only those prompts.
    pub fn search_sessions(
        &self,
        query: &str,
        since: Option<Timestamp>,
    ) -> Result<Vec<SessionRow>, AppError> {
        let fts = fts_query(query);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        self.sessions(
            "WHERE id IN (SELECT session_id FROM prompts WHERE rowid IN
                          (SELECT rowid FROM prompts_fts WHERE prompts_fts MATCH ?1))
             AND (?2 IS NULL OR ended >= ?2)
             ORDER BY started",
            params![fts, since.map(iso)],
            Some(&fts),
        )
    }

    /// Every known repo path, sorted.
    pub fn repo_paths(&self) -> Result<Vec<String>, AppError> {
        let mut stmt = self.conn.prepare("SELECT path FROM repos ORDER BY path")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Runs the session query with `tail` and loads each session's prompts, all of them or
    /// only those matching `fts`.
    fn sessions(
        &self,
        tail: &str,
        params: impl rusqlite::Params,
        fts: Option<&str>,
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
                .query_map(params![session.id, fts], |r| {
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
```

- [ ] **Step 4: Run tests and gates**

Run: `cargo nextest run store::` then `cargo clippy --all-targets` then `cargo fmt --check`
Expected: all store tests pass (5 new). Only `dead_code` warnings remain.

- [ ] **Step 5: Commit**

```bash
git add src/store.rs
git commit -m "feat: store Claude Code sessions and prompts"
```

---

### Task 3: Scan session files (`src/paths.rs`, `src/main.rs`, `tests/scan.rs`)

**Files:**
- Modify: `src/paths.rs` (new `claude_dir` + tests)
- Modify: `src/main.rs` (`run`, `scan`, new `scan_sessions`, scan output)
- Modify: `tests/scan.rs` (`Env::command` sets `CLAUDE_CONFIG_DIR`, existing scan-output asserts, new test)

**Interfaces:**
- Consumes: `claude::parse` (Task 1), `Store::{session_states, record_session}` (Task 2), existing `log_state`.
- Produces:
  - `paths::claude_dir(home: Option<&Path>, var: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf>`
  - `struct Scanned { repos: usize, commits: usize, sessions: usize, prompts: usize }` in `main.rs`
  - `fn scan(store: &mut Store, config: &Config, home: Option<&Path>, claude: Option<&Path>) -> Result<Scanned, AppError>`
  - Scan stdout: `{repos} repos, {commits} new commits, {sessions} sessions, {prompts} new prompts`

- [ ] **Step 1: Write the failing tests**

In `src/paths.rs` tests:

```rust
    #[test]
    fn claude_dir_defaults_under_home() {
        let home = abs("/home/bk");
        assert_eq!(claude_dir(Some(&home), env(&[])), Some(home.join(".claude")));
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
        assert_eq!(claude_dir(Some(&home), env(&vars)), Some(home.join(".claude")));
        assert_eq!(claude_dir(None, env(&[])), None);
    }
```

In `tests/scan.rs`, inside `Env::command`, add after the `TRAIL_HOME` line:

```rust
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
```

Add to `impl Env`:

```rust
    /// Writes a Claude Code session file whose cwd is the test repo.
    fn session_file(&self, lines: &[serde_json::Value]) -> PathBuf {
        let dir = self.root.join("claude/projects/C--test-app");
        fs::create_dir_all(dir.join("s1/subagents")).unwrap();
        // A subagent transcript must never be read.
        fs::write(
            dir.join("s1/subagents/agent-a.jsonl"),
            user_line("sub", "agent prompt", &self.repo()).to_string(),
        )
        .unwrap();
        let path = dir.join("s1.jsonl");
        let text: Vec<String> = lines.iter().map(ToString::to_string).collect();
        fs::write(&path, text.join("\n") + "\n").unwrap();
        path
    }
```

Add free functions:

```rust
fn user_line(uuid: &str, text: &str, cwd: &std::path::Path) -> serde_json::Value {
    serde_json::json!({
        "type": "user", "uuid": uuid, "timestamp": jiff::Timestamp::now().to_string(),
        "cwd": cwd.display().to_string(), "gitBranch": "main", "sessionId": "s1",
        "version": "2.1.282", "origin": {"kind": "human"}, "promptSource": "typed",
        "message": {"role": "user", "content": text}
    })
}
```

Change the existing scan-output asserts:
- `"1 repos, 9 new commits"` becomes `"1 repos, 9 new commits, 0 sessions, 0 new prompts"`
- `"1 repos, 0 new commits"` (both tests, the `scan` calls only; leave `backfill` asserts alone) becomes `"1 repos, 0 new commits, 0 sessions, 0 new prompts"`

Add the new test:

```rust
#[test]
fn scan_records_claude_sessions() {
    let env = Env::new("claude");
    env.git(&["init", "-q", "-b", "main"]);
    env.commit("one");
    let file = env.session_file(&[
        user_line("u1", "hello trail", &env.repo()),
        serde_json::json!({"type": "ai-title", "aiTitle": "Test session", "sessionId": "s1"}),
    ]);

    assert_eq!(
        env.trail(&["scan"]).trim(),
        "1 repos, 1 new commits, 1 sessions, 1 new prompts"
    );
    let db = env.db();
    assert_eq!(count(&db, "sessions"), 1);
    assert_eq!(count(&db, "prompts"), 1);
    assert_eq!(
        env.trail(&["scan"]).trim(),
        "1 repos, 0 new commits, 0 sessions, 0 new prompts"
    );

    let mut text = fs::read_to_string(&file).unwrap();
    text.push_str(&user_line("u2", "second prompt", &env.repo()).to_string());
    text.push('\n');
    fs::write(&file, text).unwrap();
    assert_eq!(
        env.trail(&["scan"]).trim(),
        "1 repos, 0 new commits, 1 sessions, 1 new prompts"
    );
    assert_eq!(count(&db, "prompts"), 2);
}

#[test]
fn scan_without_claude_dir_is_fine() {
    let env = Env::new("noclaude");
    env.git(&["init", "-q", "-b", "main"]);
    env.commit("one");
    assert_eq!(
        env.trail(&["scan"]).trim(),
        "1 repos, 1 new commits, 0 sessions, 0 new prompts"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run`
Expected: `paths` tests fail to compile (`claude_dir` missing); after stubbing, integration asserts fail on the old output format.

- [ ] **Step 3: Implement**

In `src/paths.rs`, after `resolve`:

```rust
/// Claude Code's config directory: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_dir(home: Option<&Path>, var: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    var("CLAUDE_CONFIG_DIR")
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| home.map(|h| h.join(".claude")))
}
```

In `src/main.rs`:

1. `use crate::store::{LogState, Row, Store};` stays; nothing else changes in the imports for this task.
2. At the top of `run`, after `let config = ...`:

```rust
    let claude = paths::claude_dir(home, |key| std::env::var_os(key).map(Into::into));
```

3. Every `scan(&mut store, &config, home)` call becomes `scan(&mut store, &config, home, claude.as_deref())`.
4. The `Command::Scan` arm prints:

```rust
            let s = scan(&mut store, &config, home, claude.as_deref())?;
            if !*quiet {
                println!(
                    "{} repos, {} new commits, {} sessions, {} new prompts",
                    s.repos, s.commits, s.sessions, s.prompts
                );
            }
```

5. Replace `scan` and add `Scanned` and `scan_sessions`:

```rust
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
```

6. `show` and `search` arms currently call `scan(...)?;` and discard the result; they keep doing so with the new signature.

- [ ] **Step 4: Run tests and gates**

Run: `cargo nextest run` then `cargo clippy --all-targets` then `cargo fmt --check`
Expected: all green, including `scan_records_claude_sessions` and `scan_without_claude_dir_is_fine`.

- [ ] **Step 5: Manual check against real data**

Run (bash): `TRAIL_HOME=$(mktemp -d) cargo run -- scan`
Expected: a line like `N repos, M new commits, ~600 sessions, K new prompts`, K in the low thousands. Run it again with the same `TRAIL_HOME`: `0 sessions` or only the live one. Report both lines.

- [ ] **Step 6: Commit**

```bash
git add src/paths.rs src/main.rs tests/scan.rs
git commit -m "feat: scan Claude Code sessions alongside reflogs"
```

---

### Task 4: Show sessions in `show`, `search` and `--json` (`src/view.rs`, `src/main.rs`, `tests/scan.rs`)

**Files:**
- Modify: `src/view.rs` (`Item`, `Linked`, `items`, `text`, `json`, tests)
- Modify: `src/main.rs` (`Show`/`Search` args and arms, `print_rows` becomes `print_items`)
- Modify: `tests/scan.rs` (`No commits.` assert, new asserts in `scan_records_claude_sessions`)

**Interfaces:**
- Consumes: `claude::{link, Kind}` (Task 1), `store::SessionRow`, `Store::{sessions_between, search_sessions, repo_paths}` (Task 2).
- Produces:
  - `pub enum Item { Commit(Row), Session(Linked) }`
  - `pub struct Linked { pub repo: Option<String>, pub row: SessionRow }`
  - `pub fn items(commits: Vec<Row>, sessions: Vec<SessionRow>, repos: &[String], filter: Option<&str>) -> Vec<Item>` (links sessions, filters sessions by `filter`, stable-sorts all by time)
  - `pub fn text(items: &[Item], tz: &TimeZone, prompts: bool) -> String`
  - `pub fn json(items: &[Item], tz: &TimeZone) -> serde_json::Result<String>`

- [ ] **Step 1: Write the failing tests**

In `src/view.rs` tests, update the existing calls:
- `text(&rows, &tz())` becomes `text(&items(rows, Vec::new(), &[], None), &tz(), false)`. Its snapshot must stay byte-identical.
- `text(&[], &tz())` becomes `text(&[], &tz(), false)`; its snapshot becomes `@"Nothing recorded."`.
- `json(&[r], &tz())` becomes `json(&items(vec![r], Vec::new(), &[], None), &tz())`; its snapshot gains `"type": "commit",` as the first key of the object and is otherwise unchanged.

Add helpers and tests:

```rust
    use crate::claude::{Kind, Prompt, Session};
    use crate::store::SessionRow;

    fn session_row(cwd: &str, started: &str, ended: &str, title: Option<&str>, prompts: &[(&str, Kind, &str)]) -> SessionRow {
        SessionRow {
            session: Session {
                id: format!("id-{cwd}"),
                cwd: cwd.into(),
                git_branch: "main".into(),
                title: title.map(Into::into),
                started: started.parse().unwrap(),
                ended: ended.parse().unwrap(),
                model: Some("claude-opus-5-5".into()),
                cost_usd: title.map(|_| 4.1),
                lines_added: None,
                lines_removed: None,
                cc_version: None,
            },
            prompts: prompts
                .iter()
                .enumerate()
                .map(|(i, (at, kind, text))| Prompt {
                    uuid: format!("u{i}"),
                    ts: at.parse().unwrap(),
                    kind: *kind,
                    text: (*text).into(),
                })
                .collect(),
        }
    }

    fn mixed() -> Vec<Item> {
        let commit = row("C:/a", "main", "2026-09-25T07:00:00Z", "2026-09-25T07:00:00Z", "feat: x");
        let linked = session_row(
            "C:/a/src",
            "2026-09-25T11:10:00Z",
            "2026-09-25T13:15:00Z",
            Some("Brainstorm"),
            &[
                ("2026-09-25T11:12:00Z", Kind::Typed, "first line\nsecond line"),
                ("2026-09-25T11:20:00Z", Kind::Answer, "Q: Capture?\nA: Prompts"),
            ],
        );
        let unlinked = session_row(
            "C:/b",
            "2026-09-25T08:00:00Z",
            "2026-09-25T08:30:00Z",
            None,
            &[("2026-09-25T08:00:00Z", Kind::Typed, "fix the thing")],
        );
        items(vec![commit], vec![linked, unlinked], &["C:/a".into()], None)
    }

    #[test]
    fn text_interleaves_sessions_with_commits() {
        insta::assert_snapshot!(text(&mixed(), &tz(), false), @r"
        Friday 2026-09-25
          C:/a
            09:00  main    feat: x  +10 -2
            13:10  claude  Brainstorm  2 prompts  2h05  $4.10
          C:/b
            10:00  claude  fix the thing  1 prompt  0h30
        ");
    }

    #[test]
    fn text_lists_prompts_on_request() {
        insta::assert_snapshot!(text(&mixed(), &tz(), true), @r"
        Friday 2026-09-25
          C:/a
            09:00  main    feat: x  +10 -2
            13:10  claude  Brainstorm  2 prompts  2h05  $4.10
              13:12  > first line second line
              13:20  ? Q: Capture? A: Prompts
          C:/b
            10:00  claude  fix the thing  1 prompt  0h30
              10:00  > fix the thing
        ");
    }

    #[test]
    fn long_prompts_are_clipped() {
        let long = "word ".repeat(40);
        assert_eq!(clip(&long, 10), "word word …");
        assert_eq!(clip("short", 10), "short");
    }

    #[test]
    fn repo_filter_applies_to_the_link_or_the_cwd() {
        let sessions = || {
            vec![
                session_row("C:/a/src", "2026-09-25T11:00:00Z", "2026-09-25T11:00:00Z", None, &[]),
                session_row("C:/b", "2026-09-25T12:00:00Z", "2026-09-25T12:00:00Z", None, &[]),
            ]
        };
        let repos = ["C:/a".to_owned()];
        assert_eq!(items(Vec::new(), sessions(), &repos, Some("A")).len(), 1);
        assert_eq!(items(Vec::new(), sessions(), &repos, Some("c:/b")).len(), 1);
        assert_eq!(items(Vec::new(), sessions(), &repos, Some("src")).len(), 0);
    }

    #[test]
    fn json_tags_sessions() {
        let only = items(
            Vec::new(),
            vec![session_row(
                "C:/a/src",
                "2026-09-25T11:10:00Z",
                "2026-09-25T13:15:00Z",
                Some("Brainstorm"),
                &[("2026-09-25T11:12:00Z", Kind::Typed, "first line\nsecond line")],
            )],
            &["C:/a".into()],
            None,
        );
        insta::assert_snapshot!(json(&only, &tz()).unwrap(), @r#"
        [
          {
            "type": "session",
            "repo": "C:/a",
            "cwd": "C:/a/src",
            "id": "id-C:/a/src",
            "title": "Brainstorm",
            "started": "2026-09-25T13:10:00+02:00",
            "ended": "2026-09-25T15:15:00+02:00",
            "model": "claude-opus-5-5",
            "cost_usd": 4.1,
            "lines_added": null,
            "lines_removed": null,
            "prompts": [
              {
                "ts": "2026-09-25T13:12:00+02:00",
                "kind": "typed",
                "text": "first line\nsecond line"
              }
            ]
          }
        ]
        "#);
    }
```

Note the `--repo src` case: `src` is not in the linked repo path `C:/a`, and the cwd is only used when unlinked, so it filters out. That is the spec'd rule.

In `tests/scan.rs`:
- `.contains("No commits.")` becomes `.contains("Nothing recorded.")`.
- Append to `scan_records_claude_sessions`:

```rust
    let today = env.trail(&["show", "--prompts", "--no-scan"]);
    assert!(today.contains("claude  Test session  2 prompts"), "{today}");
    assert!(today.contains("> hello trail"), "{today}");
    let json: serde_json::Value =
        serde_json::from_str(&env.trail(&["show", "--json", "--no-scan"])).unwrap();
    let items = json.as_array().unwrap();
    assert_eq!(items.len(), 2, "{json}");
    let session = items.iter().find(|i| i["type"] == "session").unwrap();
    assert!(session["repo"].as_str().unwrap().ends_with("app"), "{json}");
    assert_eq!(session["prompts"].as_array().unwrap().len(), 2);
    assert!(items.iter().any(|i| i["type"] == "commit"));
    let found = env.trail(&["search", "second", "--no-scan"]);
    assert!(found.contains("> second prompt"), "{found}");
    assert!(!found.contains("hello trail"), "{found}");
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run`
Expected: compile errors (`items`, `Item`, new `text` signature).

- [ ] **Step 3: Implement `view.rs`**

Imports: add `use crate::claude::{self, Kind};` and change `use crate::store::{FileStat, RepoSummary, Row};` to `use crate::store::{FileStat, RepoSummary, Row, SessionRow};`.

Add:

```rust
/// One line of the timeline.
#[derive(Debug)]
pub enum Item {
    Commit(Row),
    Session(Linked),
}

/// A session with the repo its cwd lies in, if trail knows one.
#[derive(Debug)]
pub struct Linked {
    pub repo: Option<String>,
    pub row: SessionRow,
}

impl Item {
    fn at(&self) -> Timestamp {
        match self {
            Self::Commit(r) => r.commit.author_date,
            Self::Session(s) => s.row.session.started,
        }
    }

    /// The repo heading it is listed under.
    fn group(&self) -> &str {
        match self {
            Self::Commit(r) => &r.repo,
            Self::Session(s) => s.repo.as_deref().unwrap_or(&s.row.session.cwd),
        }
    }

    fn branch(&self) -> &str {
        match self {
            Self::Commit(r) => &r.branch,
            Self::Session(_) => "claude",
        }
    }
}

/// Commits and sessions in one list by time. Sessions are linked to the longest enclosing
/// repo in `repos`; `filter` keeps sessions whose linked repo, or cwd when unlinked,
/// contains it (commits are already filtered by the query).
#[must_use]
pub fn items(
    commits: Vec<Row>,
    sessions: Vec<SessionRow>,
    repos: &[String],
    filter: Option<&str>,
) -> Vec<Item> {
    let filter = filter.map(str::to_lowercase);
    let mut items: Vec<Item> = commits.into_iter().map(Item::Commit).collect();
    items.extend(sessions.into_iter().filter_map(|row| {
        let repo = claude::link(&row.session.cwd, repos).map(str::to_owned);
        let key = repo.as_deref().unwrap_or(&row.session.cwd).to_lowercase();
        filter
            .as_ref()
            .is_none_or(|f| key.contains(f.as_str()))
            .then_some(Item::Session(Linked { repo, row }))
    }));
    items.sort_by_key(Item::at);
    items
}

/// Whitespace collapsed to single spaces, cut to `max` characters plus `…`.
fn clip(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        flat.chars().take(max).chain(std::iter::once('…')).collect()
    }
}

/// `  N prompts  2h05  $4.10`; cost only when known.
fn session_stats(s: &SessionRow) -> String {
    let n = s.prompts.len();
    let noun = if n == 1 { "prompt" } else { "prompts" };
    let took = s.session.ended.duration_since(s.session.started);
    let mut out = format!(
        "  {n} {noun}  {}h{:02}",
        took.as_hours(),
        took.as_mins().rem_euclid(60)
    );
    if let Some(cost) = s.session.cost_usd {
        out.push_str(&format!("  ${cost:.2}"));
    }
    out
}
```

If clippy denies `format_push_string`, use `use std::fmt::Write as _;` and `let _ = write!(out, "  ${cost:.2}");`.

Replace `text` with:

```rust
/// Day > repo > `HH:MM  branch  subject  stats`, in `tz`. Sessions show as branch `claude`
/// with their title; `prompts` lists each session's prompts under it.
#[must_use]
pub fn text(items: &[Item], tz: &TimeZone, prompts: bool) -> String {
    if items.is_empty() {
        return "Nothing recorded.\n".into();
    }
    let mut days: BTreeMap<Date, BTreeMap<&str, Vec<&Item>>> = BTreeMap::new();
    for item in items {
        let day = item.at().to_zoned(tz.clone()).date();
        days.entry(day)
            .or_default()
            .entry(item.group())
            .or_default()
            .push(item);
    }
    let hm = |ts: Timestamp| ts.to_zoned(tz.clone()).strftime("%H:%M").to_string();
    let mut lines = Vec::new();
    for (day, repos) in days {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(day.strftime("%A %Y-%m-%d").to_string());
        for (repo, group) in repos {
            let width = group
                .iter()
                .map(|i| i.branch().chars().count())
                .max()
                .unwrap_or(0);
            lines.push(format!("  {repo}"));
            for item in group {
                let (subject, tail) = match item {
                    Item::Commit(r) => (r.commit.subject.clone(), stats(&r.commit.files)),
                    Item::Session(s) => (
                        s.row.session.title.clone().unwrap_or_else(|| {
                            s.row
                                .prompts
                                .first()
                                .map_or_else(|| "(untitled)".to_owned(), |p| clip(&p.text, 80))
                        }),
                        session_stats(&s.row),
                    ),
                };
                lines.push(format!(
                    "    {}  {:<width$}  {subject}{tail}",
                    hm(item.at()),
                    item.branch(),
                ));
                if prompts && let Item::Session(s) = item {
                    for p in &s.row.prompts {
                        let mark = if p.kind == Kind::Answer { '?' } else { '>' };
                        lines.push(format!("      {}  {mark} {}", hm(p.ts), clip(&p.text, 100)));
                    }
                }
            }
        }
    }
    lines.push(String::new());
    lines.join("\n")
}
```

Items arrive sorted from `items`, so the old per-repo `sort_by_key` is gone; grouping preserves order.

Replace `JsonCommit`/`json` with:

```rust
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum JsonItem<'a> {
    Commit(JsonCommit<'a>),
    Session(JsonSession<'a>),
}

#[derive(Serialize)]
struct JsonCommit<'a> {
    repo: &'a str,
    branch: &'a str,
    hash: &'a str,
    date: String,
    subject: &'a str,
    body: &'a str,
    files: &'a [FileStat],
}

#[derive(Serialize)]
struct JsonSession<'a> {
    repo: Option<&'a str>,
    cwd: &'a str,
    id: &'a str,
    title: Option<&'a str>,
    started: String,
    ended: String,
    model: Option<&'a str>,
    cost_usd: Option<f64>,
    lines_added: Option<i64>,
    lines_removed: Option<i64>,
    prompts: Vec<JsonPrompt<'a>>,
}

#[derive(Serialize)]
struct JsonPrompt<'a> {
    ts: String,
    kind: &'static str,
    text: &'a str,
}

/// Pretty JSON array of tagged items by time, dates in `tz` with their offset. Prompts
/// carry their full text.
pub fn json(items: &[Item], tz: &TimeZone) -> serde_json::Result<String> {
    let local = |ts: Timestamp| ts.to_zoned(tz.clone()).strftime("%Y-%m-%dT%H:%M:%S%:z").to_string();
    let out: Vec<_> = items
        .iter()
        .map(|item| match item {
            Item::Commit(r) => JsonItem::Commit(JsonCommit {
                repo: &r.repo,
                branch: &r.branch,
                hash: &r.commit.hash,
                date: local(r.commit.author_date),
                subject: &r.commit.subject,
                body: &r.commit.body,
                files: &r.commit.files,
            }),
            Item::Session(s) => {
                let session = &s.row.session;
                JsonItem::Session(JsonSession {
                    repo: s.repo.as_deref(),
                    cwd: &session.cwd,
                    id: &session.id,
                    title: session.title.as_deref(),
                    started: local(session.started),
                    ended: local(session.ended),
                    model: session.model.as_deref(),
                    cost_usd: session.cost_usd,
                    lines_added: session.lines_added,
                    lines_removed: session.lines_removed,
                    prompts: s
                        .row
                        .prompts
                        .iter()
                        .map(|p| JsonPrompt {
                            ts: local(p.ts),
                            kind: p.kind.as_str(),
                            text: &p.text,
                        })
                        .collect(),
                })
            }
        })
        .collect();
    serde_json::to_string_pretty(&out)
}
```

- [ ] **Step 4: Implement `main.rs`**

Add to the `Show` variant only (after `json`). `search` always lists the matching prompts, so it gets no flag:

```rust
        /// List each Claude session's prompts under it.
        #[arg(long)]
        prompts: bool,
```

Bind `prompts` in the `Command::Show { .. }` pattern.

`Show` arm, replacing the last two lines:

```rust
            let commits = view::dedupe(store.commits_between(start, end, repo.as_deref())?);
            let sessions = store.sessions_between(start, end)?;
            let items = view::items(commits, sessions, &store.repo_paths()?, repo.as_deref());
            print_items(&items, *json, &tz, *prompts)?;
```

`Search` arm, replacing the last two lines:

```rust
            let commits = view::dedupe(store.search(query, repo.as_deref(), since)?);
            let sessions = store.search_sessions(query, since)?;
            let items = view::items(commits, sessions, &store.repo_paths()?, repo.as_deref());
            print_items(&items, *json, &tz, true)?;
```

Rename `print_rows` to `print_items`:

```rust
fn print_items(items: &[view::Item], json: bool, tz: &TimeZone, prompts: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", view::json(items, tz)?);
    } else {
        print!("{}", view::text(items, tz, prompts));
    }
    Ok(())
}
```

Remove `Row` from `use crate::store::{LogState, Row, Store};` if it is now unused. Update the `Show` doc comment to `/// Show commits and Claude sessions in a date range.` and the `Search` one to `/// Full-text search over commit subjects, bodies, changed paths and Claude prompts.`

- [ ] **Step 5: Run tests and gates; read every snapshot**

Run: `cargo nextest run` then `cargo clippy --all-targets` then `cargo fmt --check`
If an inline snapshot differs, run `cargo insta review` and compare each line with this plan's expected text. Only accept when identical in meaning; if the difference is a real bug, fix the code, not the snapshot. The existing `text_groups_by_day_then_repo` snapshot must not change.

- [ ] **Step 6: Manual check**

Run (bash, reusing the Task 3 temp home or a fresh one): `TRAIL_HOME=<dir> cargo run -- show today --prompts` and `TRAIL_HOME=<dir> cargo run -- search "claude sessions"`
Expected: today's trail session appears under the trail repo with its prompts; search shows this session with matching prompts. Paste the first ~15 lines of each in the report.

- [ ] **Step 7: Commit**

```bash
git add src/view.rs src/main.rs tests/scan.rs
git commit -m "feat: show Claude sessions next to commits in show, search and json"
```

---

### Task 5: Docs (`README.md`, `CLAUDE.md`)

**Files:**
- Modify: `README.md`
- Modify: `CLAUDE.md`

**Interfaces:** none.

- [ ] **Step 1: README**

- Intro paragraph: replace the first sentence with: `A local record of every commit you make on your machine, including the local commits that squash merging erases from remote history, and of every prompt you give Claude Code.` Keep the rest.
- After the "Why a reflog scanner" section, add:

```markdown
## Why Claude Code sessions

Most work now starts as a prompt. Claude Code keeps each session as a JSONL file under
`~/.claude/projects` (or `CLAUDE_CONFIG_DIR`) and deletes it after `cleanupPeriodDays`, 30 days
by default. trail copies what you wrote before that happens: your typed prompts, your answers
to Claude's multiple-choice questions, and each session's title, working directory, branch,
start and end, model and cost. Claude's own replies, tool output and subagent transcripts are
not stored. Sessions are shown under the repo their working directory is in.

To keep the transcripts themselves longer, set `"cleanupPeriodDays": 365` in
`~/.claude/settings.json`.
```

- Usage block: `trail show [RANGE] [--repo X] [--json] [--no-scan]` becomes `trail show [RANGE] [--repo X] [--prompts] [--json] [--no-scan]`. After the range paragraph add: `` `--prompts` lists each Claude session's prompts under it; `search` always lists the matching ones. ``

- [ ] **Step 2: CLAUDE.md**

- First line under `# trail`: `Rust CLI that records every local commit by scanning git reflogs, and every Claude Code prompt from session transcripts, into SQLite.` (rest of the paragraph unchanged).
- Architecture tree: add `  claude.rs    session JSONL -> session + prompts, cwd -> repo link [pure]` after `reflog.rs`.
- After the "Git access" section add:

```markdown
### Claude sessions

- `<CLAUDE_CONFIG_DIR or ~/.claude>/projects/*/*.jsonl`, one file per session. Subagent files
  one level deeper are never read. The format is internal: parse `serde_json::Value`, skip
  what does not fit.
- Kept: `user` lines with `origin.kind == "human"` and string content (typed), and `user` lines
  whose `toolUseResult.answers` is an object (AskUserQuestion answers). Title from the last
  `ai-title`, cost from the last `cost-state`.
- Same size+mtime skip as reflogs, in `session_state`. Sessions are linked to repos at query
  time, never stored.
```

- Storage block: add the three tables and FTS:

```sql
sessions       (id PK, cwd, git_branch, title, started, ended, model, cost_usd,
                lines_added, lines_removed, cc_version)
prompts        (uuid PK, session_id, ts, kind, text)
session_state  (path PK, size, mtime)
prompts_fts    FTS5 (text), synced by trigger
```

- Testing item 3: append `A second integration test covers Claude sessions via CLAUDE_CONFIG_DIR.`

- [ ] **Step 3: Gates and commit**

Run: `cargo fmt --check` and `cargo nextest run` (sanity; docs only).

```bash
git add README.md CLAUDE.md
git commit -m "docs: describe Claude Code session capture"
```
