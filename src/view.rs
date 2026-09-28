use std::collections::{BTreeMap, HashMap};

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Serialize;

use crate::claude::{self, Kind};
use crate::store::{FileStat, RepoSummary, Row, SessionRow};

/// Shows each piece of work once. Rows with the same author date, author email and subject
/// are versions of one commit: rebased, amended, or the same commit in another clone of the
/// repo. The newest committer date wins; on a tie, a reflog copy beats a backfilled one,
/// since the reflog knows which clone and branch it was made on.
#[must_use]
pub fn dedupe(rows: Vec<Row>) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    let mut seen: HashMap<(Timestamp, String, String), usize> = HashMap::new();
    let rank = |r: &Row| (r.commit.committer_date, r.source == "reflog");
    for row in rows {
        let key = (
            row.commit.author_date,
            row.commit.author_email.clone(),
            row.commit.subject.clone(),
        );
        if let Some(&i) = seen.get(&key) {
            if let Some(kept) = out.get_mut(i)
                && rank(&row) > rank(kept)
            {
                *kept = row;
            }
        } else {
            seen.insert(key, out.len());
            out.push(row);
        }
    }
    out
}

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
    /// Where the item sorts and which day it shows under. A session sits at the ts of the
    /// first prompt its row lists, or its start when it lists none.
    fn at(&self) -> Timestamp {
        match self {
            Self::Commit(r) => r.commit.author_date,
            Self::Session(s) => s
                .row
                .prompts
                .first()
                .map_or(s.row.session.started, |p| p.ts),
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

/// The content between a well-formed `<tag>...</tag>` pair, or `None` if the tag is missing
/// or unclosed.
fn tag_content<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let after_open = text.split_once(&format!("<{tag}>"))?.1;
    after_open
        .split_once(&format!("</{tag}>"))
        .map(|(content, _)| content)
}

/// Slash-command prompts are stored as raw `<command-name>`/`<command-args>` XML (tags may
/// appear in any order). This renders them as `NAME ARGS` for display, dropping empty args.
/// Plain prompts, and malformed XML (an unclosed tag), are returned unchanged.
#[must_use]
fn command_display(text: &str) -> String {
    let Some(name) = tag_content(text, "command-name") else {
        return text.to_owned();
    };
    match tag_content(text, "command-args").map(str::trim) {
        Some(args) if !args.is_empty() => format!("{name} {args}"),
        _ => name.to_owned(),
    }
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
        use std::fmt::Write as _;
        let _ = write!(out, "  ${cost:.2}");
    }
    out
}

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
                            s.row.prompts.first().map_or_else(
                                || "(untitled)".to_owned(),
                                |p| {
                                    let display = command_display(&p.text);
                                    clip(display.lines().next().unwrap_or(""), 80)
                                },
                            )
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
                        lines.push(format!(
                            "      {}  {mark} {}",
                            hm(p.ts),
                            clip(&command_display(&p.text), 100)
                        ));
                    }
                }
            }
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

/// `  +N -M`, or nothing for commits without files (merges).
fn stats(files: &[FileStat]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let sum = |f: fn(&FileStat) -> Option<i64>| {
        files.iter().filter_map(f).fold(0i64, i64::saturating_add)
    };
    format!("  +{} -{}", sum(|f| f.insertions), sum(|f| f.deletions))
}

/// `COUNT  LAST-DAY  PATH` per repo, counts right-aligned.
#[must_use]
pub fn repos_text(repos: &[RepoSummary], tz: &TimeZone) -> String {
    let width = repos
        .iter()
        .map(|r| r.commits.to_string().len())
        .max()
        .unwrap_or(0);
    repos
        .iter()
        .map(|r| {
            let last = r.last.map_or_else(
                || "-".to_owned(),
                |t| t.to_zoned(tz.clone()).date().to_string(),
            );
            format!("{:>width$}  {last:<10}  {}\n", r.commits, r.path)
        })
        .collect::<Vec<_>>()
        .concat()
}

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
    let local = |ts: Timestamp| {
        ts.to_zoned(tz.clone())
            .strftime("%Y-%m-%dT%H:%M:%S%:z")
            .to_string()
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Commit;

    fn row(repo: &str, branch: &str, author: &str, committer: &str, subject: &str) -> Row {
        Row {
            repo: repo.into(),
            branch: branch.into(),
            source: "reflog".into(),
            commit: Commit {
                hash: format!("{:0>40}", subject.len()),
                author_name: "BK".into(),
                author_email: "bk@example.com".into(),
                author_date: author.parse().unwrap(),
                committer_date: committer.parse().unwrap(),
                subject: subject.into(),
                body: String::new(),
                files: vec![
                    FileStat {
                        path: "src/a.rs".into(),
                        insertions: Some(10),
                        deletions: Some(2),
                    },
                    FileStat {
                        path: "logo.png".into(),
                        insertions: None,
                        deletions: None,
                    },
                ],
            },
        }
    }

    fn tz() -> TimeZone {
        TimeZone::fixed(jiff::tz::offset(2))
    }

    #[test]
    fn dedupe_keeps_newest_version() {
        let rows = vec![
            row(
                "C:/a",
                "main",
                "2026-09-25T07:00:00Z",
                "2026-09-25T07:00:00Z",
                "feat: x",
            ),
            row(
                "C:/a",
                "main",
                "2026-09-25T07:00:00Z",
                "2026-09-25T09:00:00Z",
                "feat: x",
            ),
            row(
                "C:/a",
                "main",
                "2026-09-25T07:00:00Z",
                "2026-09-25T08:00:00Z",
                "feat: x",
            ),
            row(
                "C:/a",
                "main",
                "2026-09-25T07:00:00Z",
                "2026-09-25T07:00:00Z",
                "feat: y",
            ),
        ];
        let kept: Vec<_> = dedupe(rows)
            .into_iter()
            .map(|r| (r.commit.subject, r.commit.committer_date.to_string()))
            .collect();
        assert_eq!(
            kept,
            [
                ("feat: x".to_owned(), "2026-09-25T09:00:00Z".to_owned()),
                ("feat: y".to_owned(), "2026-09-25T07:00:00Z".to_owned()),
            ]
        );
    }

    #[test]
    fn dedupe_across_clones_prefers_the_reflog_copy() {
        let mut clone = row(
            "C:/c4/kai",
            "HEAD",
            "2026-09-25T07:00:00Z",
            "2026-09-25T07:00:00Z",
            "feat: x",
        );
        clone.source = "backfill".into();
        let mine = row(
            "C:/kai",
            "feat/x",
            "2026-09-25T07:00:00Z",
            "2026-09-25T07:00:00Z",
            "feat: x",
        );
        let mut other_author = row(
            "C:/c4/kai",
            "HEAD",
            "2026-09-25T07:00:00Z",
            "2026-09-25T07:00:00Z",
            "feat: x",
        );
        other_author.commit.author_email = "someone@example.com".into();
        let kept: Vec<_> = dedupe(vec![clone, mine, other_author])
            .into_iter()
            .map(|r| (r.repo, r.branch, r.commit.author_email))
            .collect();
        assert_eq!(
            kept,
            [
                (
                    "C:/kai".to_owned(),
                    "feat/x".to_owned(),
                    "bk@example.com".to_owned()
                ),
                (
                    "C:/c4/kai".to_owned(),
                    "HEAD".to_owned(),
                    "someone@example.com".to_owned()
                ),
            ]
        );
    }

    use crate::claude::{Kind, Prompt, Session};
    use crate::store::SessionRow;

    fn session_row(
        cwd: &str,
        started: &str,
        ended: &str,
        title: Option<&str>,
        prompts: &[(&str, Kind, &str)],
    ) -> SessionRow {
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
        let commit = row(
            "C:/a",
            "main",
            "2026-09-25T07:00:00Z",
            "2026-09-25T07:00:00Z",
            "feat: x",
        );
        let linked = session_row(
            "C:/a/src",
            "2026-09-25T11:10:00Z",
            "2026-09-25T13:15:00Z",
            Some("Brainstorm"),
            &[
                (
                    "2026-09-25T11:12:00Z",
                    Kind::Typed,
                    "first line\nsecond line",
                ),
                (
                    "2026-09-25T11:20:00Z",
                    Kind::Answer,
                    "Q: Capture?\nA: Prompts",
                ),
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
            13:12  claude  Brainstorm  2 prompts  2h05  $4.10
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
            13:12  claude  Brainstorm  2 prompts  2h05  $4.10
              13:12  > first line second line
              13:20  ? Q: Capture? A: Prompts
          C:/b
            10:00  claude  fix the thing  1 prompt  0h30
              10:00  > fix the thing
        ");
    }

    #[test]
    fn session_is_placed_by_its_first_listed_prompt() {
        let spanning = session_row(
            "C:/a",
            "2026-09-24T23:00:00Z",
            "2026-09-25T09:00:00Z",
            None,
            &[("2026-09-25T08:00:00Z", Kind::Typed, "only prompt in range")],
        );
        let items = items(Vec::new(), vec![spanning], &["C:/a".into()], None);
        insta::assert_snapshot!(text(&items, &tz(), false), @r"
        Friday 2026-09-25
          C:/a
            10:00  claude  only prompt in range  1 prompt  10h00
        ");
    }

    #[test]
    fn untitled_session_falls_back_to_the_first_prompt_line() {
        let session = session_row(
            "C:/a",
            "2026-09-25T11:00:00Z",
            "2026-09-25T11:05:00Z",
            None,
            &[(
                "2026-09-25T11:00:00Z",
                Kind::Typed,
                "record claude sessions\nas a second line",
            )],
        );
        let items = items(Vec::new(), vec![session], &["C:/a".into()], None);
        insta::assert_snapshot!(text(&items, &tz(), false), @r"
        Friday 2026-09-25
          C:/a
            13:00  claude  record claude sessions  1 prompt  0h05
        ");
    }

    #[test]
    fn long_prompts_are_clipped() {
        let long = "word ".repeat(40);
        assert_eq!(clip(&long, 10), "word word …");
        assert_eq!(clip("short", 10), "short");
    }

    #[test]
    fn command_display_shows_name_and_args() {
        let text = "<command-message>caveman:caveman</command-message>\n\
            <command-name>/caveman:caveman</command-name>\n\
            <command-args>Run bd ready</command-args>";
        assert_eq!(command_display(text), "/caveman:caveman Run bd ready");
    }

    #[test]
    fn command_display_without_args_tag() {
        let text = "<command-name>/caveman:caveman</command-name>";
        assert_eq!(command_display(text), "/caveman:caveman");
    }

    #[test]
    fn command_display_with_empty_args() {
        let text = "<command-name>/caveman:caveman</command-name><command-args></command-args>";
        assert_eq!(command_display(text), "/caveman:caveman");
    }

    #[test]
    fn command_display_handles_tags_out_of_order() {
        let text = "<command-args>Run bd ready</command-args><command-name>/caveman:caveman</command-name>";
        assert_eq!(command_display(text), "/caveman:caveman Run bd ready");
    }

    #[test]
    fn command_display_leaves_plain_prompt_unchanged() {
        assert_eq!(command_display("fix the thing"), "fix the thing");
    }

    #[test]
    fn command_display_leaves_malformed_xml_unchanged() {
        let text = "<command-name>/caveman:caveman";
        assert_eq!(command_display(text), text);
    }

    #[test]
    fn repo_filter_applies_to_the_link_or_the_cwd() {
        let sessions = || {
            vec![
                session_row(
                    "C:/a/src",
                    "2026-09-25T11:00:00Z",
                    "2026-09-25T11:00:00Z",
                    None,
                    &[],
                ),
                session_row(
                    "C:/b",
                    "2026-09-25T12:00:00Z",
                    "2026-09-25T12:00:00Z",
                    None,
                    &[],
                ),
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
                &[(
                    "2026-09-25T11:12:00Z",
                    Kind::Typed,
                    "first line\nsecond line",
                )],
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

    #[test]
    fn text_groups_by_day_then_repo() {
        let mut merge = row(
            "C:/b",
            "main",
            "2026-09-25T22:30:00Z",
            "2026-09-25T22:30:00Z",
            "Merge branch 'x'",
        );
        merge.commit.files.clear();
        let rows = vec![
            row(
                "C:/b",
                "main",
                "2026-09-25T06:05:00Z",
                "2026-09-25T06:05:00Z",
                "feat: first",
            ),
            row(
                "C:/a",
                "feat/long",
                "2026-09-25T07:10:00Z",
                "2026-09-25T07:10:00Z",
                "fix: second",
            ),
            row(
                "C:/a",
                "main",
                "2026-09-25T08:00:00Z",
                "2026-09-25T08:00:00Z",
                "docs: third",
            ),
            merge,
        ];
        insta::assert_snapshot!(text(&items(rows, Vec::new(), &[], None), &tz(), false), @r"
        Friday 2026-09-25
          C:/a
            09:10  feat/long  fix: second  +10 -2
            10:00  main       docs: third  +10 -2
          C:/b
            08:05  main  feat: first  +10 -2

        Saturday 2026-09-26
          C:/b
            00:30  main  Merge branch 'x'
        ");
    }

    #[test]
    fn repos_table() {
        let repos = [
            RepoSummary {
                path: "C:/src/a".into(),
                commits: 120,
                last: Some("2026-09-25T22:30:00Z".parse().unwrap()),
            },
            RepoSummary {
                path: "C:/src/empty".into(),
                commits: 0,
                last: None,
            },
        ];
        insta::assert_snapshot!(repos_text(&repos, &tz()), @r"
        120  2026-09-26  C:/src/a
          0  -           C:/src/empty
        ");
    }

    #[test]
    fn text_without_commits() {
        insta::assert_snapshot!(text(&[], &tz(), false), @"Nothing recorded.");
    }

    #[test]
    fn json_has_local_dates_bodies_and_files() {
        let mut r = row(
            "C:/a",
            "main",
            "2026-09-25T07:00:00Z",
            "2026-09-25T07:00:00Z",
            "feat: x",
        );
        r.commit.body = "Why.".into();
        let only = items(vec![r], Vec::new(), &[], None);
        insta::assert_snapshot!(json(&only, &tz()).unwrap(), @r#"
        [
          {
            "type": "commit",
            "repo": "C:/a",
            "branch": "main",
            "hash": "0000000000000000000000000000000000000007",
            "date": "2026-09-25T09:00:00+02:00",
            "subject": "feat: x",
            "body": "Why.",
            "files": [
              {
                "path": "src/a.rs",
                "insertions": 10,
                "deletions": 2
              },
              {
                "path": "logo.png",
                "insertions": null,
                "deletions": null
              }
            ]
          }
        ]
        "#);
    }
}
