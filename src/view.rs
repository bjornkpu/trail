use std::collections::{BTreeMap, HashMap};

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Serialize;

use crate::store::{FileStat, RepoSummary, Row};

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

/// Day > repo > `HH:MM  branch  subject  +N -M`, in `tz`, by author date.
#[must_use]
pub fn text(rows: &[Row], tz: &TimeZone) -> String {
    if rows.is_empty() {
        return "No commits.\n".into();
    }
    let mut days: BTreeMap<Date, BTreeMap<&str, Vec<&Row>>> = BTreeMap::new();
    for row in rows {
        let day = row.commit.author_date.to_zoned(tz.clone()).date();
        days.entry(day)
            .or_default()
            .entry(&row.repo)
            .or_default()
            .push(row);
    }
    let mut lines = Vec::new();
    for (day, repos) in days {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(day.strftime("%A %Y-%m-%d").to_string());
        for (repo, mut commits) in repos {
            commits.sort_by_key(|r| r.commit.author_date);
            let width = commits
                .iter()
                .map(|r| r.branch.chars().count())
                .max()
                .unwrap_or(0);
            lines.push(format!("  {repo}"));
            for r in commits {
                let time = r.commit.author_date.to_zoned(tz.clone());
                lines.push(format!(
                    "    {}  {:<width$}  {}{}",
                    time.strftime("%H:%M"),
                    r.branch,
                    r.commit.subject,
                    stats(&r.commit.files)
                ));
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
struct JsonCommit<'a> {
    repo: &'a str,
    branch: &'a str,
    hash: &'a str,
    date: String,
    subject: &'a str,
    body: &'a str,
    files: &'a [FileStat],
}

/// Pretty JSON array, dates in `tz` with their offset.
pub fn json(rows: &[Row], tz: &TimeZone) -> serde_json::Result<String> {
    let commits: Vec<_> = rows
        .iter()
        .map(|r| JsonCommit {
            repo: &r.repo,
            branch: &r.branch,
            hash: &r.commit.hash,
            date: r
                .commit
                .author_date
                .to_zoned(tz.clone())
                .strftime("%Y-%m-%dT%H:%M:%S%:z")
                .to_string(),
            subject: &r.commit.subject,
            body: &r.commit.body,
            files: &r.commit.files,
        })
        .collect();
    serde_json::to_string_pretty(&commits)
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
        let rows = [
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
        insta::assert_snapshot!(text(&rows, &tz()), @r"
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
        insta::assert_snapshot!(text(&[], &tz()), @"No commits.");
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
        insta::assert_snapshot!(json(&[r], &tz()).unwrap(), @r#"
        [
          {
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
