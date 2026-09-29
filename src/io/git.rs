use std::path::Path;
use std::process::Command;

use crate::domain::view::{Commit, FileStat};

/// Header fields, each NUL-terminated, then `--numstat` lines.
const FORMAT: &str = "--format=%H%x00%an%x00%ae%x00%aI%x00%cI%x00%s%x00%b%x00";

/// Reads one commit's details and changed files with `git show`. Merge commits get no
/// files. The error is a message for the log: git missing, object gone, bad output.
// ponytail: one git process per unseen commit; batch hashes into one `git log --no-walk`
// if the first scan of a big machine gets slow.
pub fn show(repo: &Path, hash: &str) -> Result<Commit, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.quotepath=off", "show", "--no-renames"])
        .args(["--diff-merges=off", "--numstat", FORMAT, hash, "--"])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    parse_show(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| format!("unexpected git show output for {hash}"))
}

/// Every commit reachable from any ref whose author email is one of `emails`, with files.
pub fn log_by_authors(repo: &Path, emails: &[String]) -> Result<Vec<Commit>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.quotepath=off", "log", "--all", "--no-renames"])
        .args(["--diff-merges=off", "--numstat", "--regexp-ignore-case"])
        .arg(FORMAT.replacen("--format=", "--format=%x1e", 1))
        .args(
            emails
                .iter()
                .map(|e| format!("--author={}", author_pattern(e))),
        )
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    Ok(parse_log(&String::from_utf8_lossy(&out.stdout)))
}

/// `git log` output with each commit starting at a record separator. Unreadable records
/// are dropped.
fn parse_log(out: &str) -> Vec<Commit> {
    out.split('\x1e').filter_map(parse_show).collect()
}

/// `--author` takes a regex over `Name <email>`; match the email exactly.
fn author_pattern(email: &str) -> String {
    let mut pattern = String::from("<");
    for c in email.chars() {
        if ".+*?[](){}^$|\\".contains(c) {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('>');
    pattern
}

#[must_use]
fn parse_show(out: &str) -> Option<Commit> {
    let mut fields = out.splitn(8, '\0');
    let mut next = || fields.next();
    let (hash, name, email, author, committer, subject, body) = (
        next()?,
        next()?,
        next()?,
        next()?,
        next()?,
        next()?,
        next()?,
    );
    let numstat = next()?;
    let files = numstat
        .lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let mut parts = line.splitn(3, '\t');
            let (ins, del, path) = (parts.next()?, parts.next()?, parts.next()?);
            Some(FileStat {
                path: path.into(),
                // Binary files show `-` for both counts.
                insertions: ins.parse().ok(),
                deletions: del.parse().ok(),
            })
        })
        .collect::<Option<_>>()?;
    Some(Commit {
        hash: hash.into(),
        author_name: name.into(),
        author_email: email.into(),
        author_date: author.parse().ok()?,
        committer_date: committer.parse().ok()?,
        subject: subject.into(),
        body: body.trim_end().into(),
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commit_with_files() {
        let out = "250bef217e13f9c6cf3145d14da3d88126a06798\0Bjørn Kristian Punsvik\0bk@example.com\0\
                   2026-09-25T14:18:37+02:00\x002026-09-25T14:18:38+02:00\0feat: add æøå\0Body line.\n\n\0\n\
                   \n10\t2\tsrc/æøå.rs\n-\t-\tlogo.png\n";
        let c = parse_show(out).unwrap();
        assert_eq!(c.hash, "250bef217e13f9c6cf3145d14da3d88126a06798");
        assert_eq!(c.author_name, "Bjørn Kristian Punsvik");
        assert_eq!(c.author_date.to_string(), "2026-09-25T12:18:37Z");
        assert_eq!(c.committer_date.to_string(), "2026-09-25T12:18:38Z");
        assert_eq!(c.subject, "feat: add æøå");
        assert_eq!(c.body, "Body line.");
        assert_eq!(
            c.files,
            [
                FileStat {
                    path: "src/æøå.rs".into(),
                    insertions: Some(10),
                    deletions: Some(2),
                },
                FileStat {
                    path: "logo.png".into(),
                    insertions: None,
                    deletions: None,
                },
            ]
        );
    }

    #[test]
    fn merge_without_files() {
        let out = "edb48d2d1375e3d26ed77e40506aea911c08cefc\0BK\0bk@example.com\0\
                   2026-09-25T14:18:37+02:00\x002026-09-25T14:18:37+02:00\0Merge branch 'c3'\0\0\n";
        let c = parse_show(out).unwrap();
        assert_eq!(c.body, "");
        assert!(c.files.is_empty());
    }

    #[test]
    fn truncated_output_is_none() {
        assert!(parse_show("250bef2\0BK\0").is_none());
        assert!(parse_show("").is_none());
    }

    #[test]
    fn log_output_splits_on_record_separator() {
        let out = "\x1eaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0BK\0bk@example.com\0\
                   2026-05-01T10:00:00+02:00\x002026-05-01T10:00:00+02:00\0feat: a\0\0\n\
                   \n1\t0\ta.txt\n\
                   \x1ebbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\0BK\0bk@example.com\0\
                   2026-05-02T10:00:00+02:00\x002026-05-02T10:00:00+02:00\0Merge x\0\0\n\
                   \x1egarbage\n";
        let commits = parse_log(out);
        let subjects: Vec<_> = commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["feat: a", "Merge x"]);
        assert_eq!(commits[0].files.len(), 1);
        assert!(commits[1].files.is_empty());
    }

    #[test]
    fn author_pattern_matches_email_literally() {
        assert_eq!(
            author_pattern("first.last@example.com"),
            r"<first\.last@example\.com>"
        );
    }
}
