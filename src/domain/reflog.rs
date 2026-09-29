/// One reflog line: `<old> <new> <name> <<email>> <unix-ts> <tz>\t<op>: <message>`.
/// Only the parts trail stores are kept.
#[derive(Debug, PartialEq, Eq)]
pub struct ReflogEntry {
    pub hash: String,
    pub ts: i64,
    pub op: String,
    pub message: String,
}

/// Parses one reflog line, or `None` when it is malformed.
#[must_use]
pub fn parse_line(line: &str) -> Option<ReflogEntry> {
    let (header, text) = line.split_once('\t')?;
    let (old, rest) = header.split_once(' ')?;
    let (new, rest) = rest.split_once(' ')?;
    let mut tail = rest.rsplitn(3, ' ');
    let (_tz, ts, ident) = (tail.next()?, tail.next()?, tail.next()?);
    if !is_hash(old) || !is_hash(new) || !ident.ends_with('>') || !ident.contains('<') {
        return None;
    }
    // Ops like `pull --rebase C:/src/up main (pick)` contain a colon, but never ": ".
    let (op, message) = text.split_once(": ").unwrap_or((text, ""));
    Some(ReflogEntry {
        hash: new.into(),
        ts: ts.parse().ok()?,
        op: op.into(),
        message: message.into(),
    })
}

/// SHA-1 or SHA-256 object id.
fn is_hash(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether a reflog entry records git creating a new commit.
#[must_use]
pub fn creates_commit(op: &str, message: &str) -> bool {
    if matches!(
        op,
        "commit"
            | "commit (initial)"
            | "commit (amend)"
            | "commit (merge)"
            | "commit (cherry-pick)"
            | "cherry-pick"
            | "revert"
    ) {
        return true;
    }
    // Rebase steps: `rebase (pick)`, `rebase -i (pick)`, `pull --rebase origin main (pick)`.
    if let Some((cmd, step)) = op.strip_suffix(')').and_then(|o| o.rsplit_once(" (")) {
        return (is_git(cmd, "rebase") || is_git(cmd, "pull"))
            && matches!(
                step,
                "pick" | "reword" | "squash" | "fixup" | "edit" | "continue" | "merge"
            );
    }
    (is_git(op, "merge") || is_git(op, "pull")) && message.starts_with("Merge made by")
}

/// `op` is the git command `cmd`, with or without arguments.
fn is_git(op: &str, cmd: &str) -> bool {
    op.strip_prefix(cmd)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real reflog lines from git 2.55, with emails masked and messages anonymised.
    const COMMIT: &str = "b7f4b187bd3f229791aa2b0dd11439278dffeb27 45aa997bacc27b978384a555e05eabdfea51d5b3 Bjørn Kristian Punsvik <bk@example.com> 1787681201 +0200\tcommit: fix: reserve stdout for the worktree path; stop seeding secrets into review trees";
    const UPDATE_BY_PUSH: &str = "0000000000000000000000000000000000000000 f4aaccb8ff23a79166daef16be7354f5344fcf27 Bjørn Kristian Punsvik <bk@example.com> 1790334226 +0200\tupdate by push";
    // Real line with the long scratch path shortened; the colon in `C:/` must not end the op.
    const PULL_REBASE_WINDOWS_PATH: &str = "e32f8893544000368bb63b616ab3d8065fad081a ff21738daff2304c7f48bb8c8b73c0e7220d8007 Bjørn Kristian Punsvik <bk@example.com> 1790338616 +0200\tpull -q --rebase C:/src/up main (start): checkout ff21738daff2304c7f48bb8c8b73c0e7220d8007";

    #[test]
    fn parses_commit_line() {
        assert_eq!(
            parse_line(COMMIT),
            Some(ReflogEntry {
                hash: "45aa997bacc27b978384a555e05eabdfea51d5b3".into(),
                ts: 1_787_681_201,
                op: "commit".into(),
                message: "fix: reserve stdout for the worktree path; stop seeding secrets into review trees".into(),
            })
        );
    }

    #[test]
    fn op_without_message() {
        let entry = parse_line(UPDATE_BY_PUSH).unwrap();
        assert_eq!(entry.op, "update by push");
        assert_eq!(entry.message, "");
    }

    #[test]
    fn colon_inside_op_does_not_split_it() {
        let entry = parse_line(PULL_REBASE_WINDOWS_PATH).unwrap();
        assert_eq!(entry.op, "pull -q --rebase C:/src/up main (start)");
        assert_eq!(
            entry.message,
            "checkout ff21738daff2304c7f48bb8c8b73c0e7220d8007"
        );
    }

    #[test]
    fn empty_author_name_parses() {
        let line = "0000000000000000000000000000000000000000 45aa997bacc27b978384a555e05eabdfea51d5b3 <bk@example.com> 1787681201 +0200\tcommit: x";
        assert_eq!(parse_line(line).unwrap().op, "commit");
    }

    #[test]
    fn malformed_lines_are_none() {
        for line in [
            "",
            "garbage",
            COMMIT.split_once('\t').unwrap().0,          // no tab
            &COMMIT.replacen("45aa997b", "zzzzzzzz", 1), // bad new hash
            &COMMIT.replacen("b7f4b187bd", "b7f4", 1),   // short old hash
            &COMMIT.replacen("1787681201", "soon", 1),   // bad timestamp
            &COMMIT.replacen("<bk@example.com>", "bk@example.com", 1), // no email brackets
        ] {
            assert_eq!(parse_line(line), None, "{line}");
        }
    }

    // The op and message halves of real lines: (op, message, kept).
    const OPS: &[(&str, &str, bool)] = &[
        ("commit", "fix: reserve stdout for the worktree path", true),
        ("commit (initial)", "wt: initial import", true),
        (
            "commit (amend)",
            "fix: give each agent session its own worktree",
            true,
        ),
        (
            "commit (merge)",
            "Merge remote-tracking branch 'origin/main' into chore/sql",
            true,
        ),
        ("commit (cherry-pick)", "s1", true),
        (
            "cherry-pick",
            "feat(api): allow the remaining source systems",
            true,
        ),
        ("revert", "Revert \"four\"", true),
        ("rebase (pick)", "Add changelog fragment", true),
        ("rebase (reword)", "three", true),
        (
            "rebase (squash)",
            "# This is a combination of 2 commits.",
            true,
        ),
        (
            "rebase (fixup)",
            "Skip the transform phase when none are configured",
            true,
        ),
        ("rebase (edit)", "e", true),
        ("rebase (continue)", "a", true),
        ("rebase (merge)", "Merge branch 'm1' into m2", true),
        (
            "rebase -i (pick)",
            "older git names interactive rebases this way",
            true,
        ),
        ("pull -q --rebase ../up main (pick)", "local1", true),
        (
            "merge add-local-model",
            "Merge made by the 'ort' strategy.",
            true,
        ),
        (
            "pull -q --no-rebase --no-edit ../up main",
            "Merge made by the 'ort' strategy.",
            true,
        ),
        ("merge ci-and-readme", "Fast-forward", false),
        ("pull --ff-only origin main", "Fast-forward", false),
        ("pull -q", "fast-forward", false),
        ("rebase (start)", "checkout origin/main", false),
        (
            "rebase (finish)",
            "returning to refs/heads/skip-empty-transforms",
            false,
        ),
        ("rebase (reset)", "'3b6264c'", false),
        ("rebase", "fast-forward", false),
        (
            "pull -q --rebase ../up main (start)",
            "checkout ff21738daff2304c7f48bb8c8b73c0e7220d8007",
            false,
        ),
        (
            "pull -q --rebase ../up main (finish)",
            "returning to refs/heads/main",
            false,
        ),
        ("checkout", "moving from main to chore/flip-dns-lock", false),
        ("reset", "moving to HEAD", false),
        (
            "clone",
            "from ssh.dev.azure.com:v3/example/Project/platform-management",
            false,
        ),
        ("branch", "Created from HEAD", false),
        (
            "Branch",
            "renamed refs/heads/feat/usage-metrics to refs/heads/feat/1234-usage-metrics",
            false,
        ),
        ("fetch origin main", "fast-forward", false),
        ("update by push", "", false),
        ("autostash", "", false),
        ("mergetool (pick)", "not a rebase", false),
    ];

    #[test]
    fn keeps_only_commit_creating_ops() {
        for &(op, message, kept) in OPS {
            assert_eq!(creates_commit(op, message), kept, "{op}: {message}");
        }
    }
}
