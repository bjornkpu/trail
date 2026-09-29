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
        if s == "answer" {
            Self::Answer
        } else {
            Self::Typed
        }
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
        let (kind, text) = if str_at(v, "/origin/kind") == Some("human") {
            let Some(text) = human_text(v) else {
                return;
            };
            (Kind::Typed, text)
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

/// The typed text of a human `user` line: the string content, or, when a pasted image makes
/// `message.content` an array of blocks, its `text` blocks joined with `\n`. `None` when
/// there is no text at all (an image-only prompt).
fn human_text(v: &Value) -> Option<String> {
    if let Some(text) = str_at(v, "/message/content") {
        return Some(text.to_owned());
    }
    let blocks = v.pointer("/message/content")?.as_array()?;
    let text = blocks
        .iter()
        .filter(|b| str_at(b, "/type") == Some("text"))
        .filter_map(|b| str_at(b, "/text"))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
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
            Some(notes.map_or_else(
                || format!("Q: {question}\nA: {answer}"),
                |notes| format!("Q: {question}\nA: {answer}\nNotes: {notes}"),
            ))
        })
        .collect();
    (!blocks.is_empty()).then(|| blocks.join("\n\n"))
}

/// The longest repo path that is `cwd` or encloses it. ASCII case and `/` versus `\` are
/// ignored, since Windows reports the same directory both ways.
#[must_use]
pub fn link<'a>(cwd: &str, repos: &'a [String]) -> Option<&'a str> {
    let norm = |p: &str| {
        p.replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
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
    const IMAGE: &str = r#"{"type":"user","uuid":"u8","timestamp":"2026-09-28T11:18:00.000Z","cwd":"C:\\x\\trail","sessionId":"s1","origin":{"kind":"human"},"message":{"role":"user","content":[{"type":"text","text":"check this screenshot"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}}]}}"#;
    const IMAGE_ONLY: &str = r#"{"type":"user","uuid":"u9","timestamp":"2026-09-28T11:19:00.000Z","cwd":"C:\\x\\trail","sessionId":"s1","origin":{"kind":"human"},"message":{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}}]}}"#;
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
        let parsed = parse(&file(&[
            TYPED,
            PEER,
            NOTIFY,
            SKILL,
            META,
            TOOL_RESULT,
            ASSISTANT,
        ]));
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
    fn typed_prompt_with_image_keeps_the_text_blocks() {
        let parsed = parse(&file(&[IMAGE]));
        assert_eq!(parsed.prompts.len(), 1);
        let p = parsed.prompts.first().unwrap();
        assert_eq!(p.kind, Kind::Typed);
        assert_eq!(p.text, "check this screenshot");
    }

    #[test]
    fn image_only_prompt_is_dropped() {
        let parsed = parse(&file(&[IMAGE_ONLY]));
        assert!(parsed.prompts.is_empty());
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
