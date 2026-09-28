// Marks this file as test code, so clippy.toml allows unwrap in its helpers too.
#![cfg(test)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

struct Env {
    root: PathBuf,
}

impl Env {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("trail-it-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/app")).unwrap();
        fs::create_dir_all(root.join("trail")).unwrap();
        fs::write(root.join("gitconfig"), "").unwrap();
        fs::write(
            root.join("trail/config.toml"),
            format!("roots = ['{}']\n", root.join("src").display()),
        )
        .unwrap();
        Self { root }
    }

    fn repo(&self) -> PathBuf {
        self.root.join("src/app")
    }

    /// Isolated from the user's git config, so hooks and signing never run.
    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(self.repo())
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "BK")
            .env("GIT_AUTHOR_EMAIL", "bk@example.com")
            .env("GIT_COMMITTER_NAME", "BK")
            .env("GIT_COMMITTER_EMAIL", "bk@example.com")
            .env("TRAIL_HOME", self.root.join("trail"));
        cmd
    }

    fn git(&self, args: &[&str]) -> String {
        let out = self.command("git").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn commit(&self, name: &str) -> String {
        fs::write(self.repo().join(format!("{name}.txt")), name).unwrap();
        self.git(&["add", "."]);
        self.git(&["commit", "-qm", name]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn trail(&self, args: &[&str]) -> String {
        let out = self
            .command(env!("CARGO_BIN_EXE_trail"))
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "trail {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.join("trail/trail.db")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn count(db: &rusqlite::Connection, table: &str) -> i64 {
    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn stored(db: &rusqlite::Connection, hash: &str) -> bool {
    db.query_row(
        "SELECT count(*) FROM commits WHERE hash = ?1",
        [hash],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        == 1
}

#[test]
fn scan_keeps_every_local_commit() {
    let env = Env::new("scan");
    env.git(&["init", "-q", "-b", "main"]);
    let mut hashes = vec![env.commit("one"), env.commit("two")];
    env.git(&["commit", "-q", "--amend", "-m", "two amended"]);
    hashes.push(env.git(&["rev-parse", "HEAD"]));

    env.git(&["checkout", "-q", "-b", "feat"]);
    hashes.push(env.commit("f1"));
    hashes.push(env.commit("f2"));
    env.git(&["checkout", "-q", "main"]);
    hashes.push(env.commit("three"));
    env.git(&["checkout", "-q", "feat"]);
    env.git(&["rebase", "-q", "main"]);
    hashes.push(env.git(&["rev-parse", "HEAD~1"]));
    hashes.push(env.git(&["rev-parse", "HEAD"]));

    env.git(&["checkout", "-q", "main"]);
    env.git(&["merge", "-q", "--squash", "feat"]);
    env.git(&["commit", "-qm", "squash feat"]);
    hashes.push(env.git(&["rev-parse", "HEAD"]));
    env.git(&["branch", "-q", "-D", "feat"]);

    let out = env.trail(&["scan"]);
    assert_eq!(out.trim(), "1 repos, 9 new commits");
    let db = env.db();
    for hash in &hashes {
        assert!(stored(&db, hash), "{hash} not stored");
    }
    let (commits, entries) = (count(&db, "commits"), count(&db, "reflog_entries"));
    assert_eq!(commits, 9);

    assert_eq!(env.trail(&["scan"]).trim(), "1 repos, 0 new commits");
    assert_eq!(env.trail(&["scan", "--quiet"]), "");
    assert_eq!(count(&db, "commits"), commits);
    assert_eq!(count(&db, "reflog_entries"), entries);
    assert!(env.root.join("trail/trail.log").is_file());

    // Rebased f1 collapses into one line; the amended commit changed subject, so both stay.
    let today = env.trail(&["show", "--no-scan"]);
    assert_eq!(today.matches("  f1").count(), 1, "{today}");
    for subject in ["one", "two", "two amended", "f2", "three", "squash feat"] {
        assert!(
            today.contains(&format!("  {subject}")),
            "{subject}: {today}"
        );
    }
    let json: serde_json::Value =
        serde_json::from_str(&env.trail(&["show", "today", "--json", "--no-scan"])).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 7);
    assert!(
        env.trail(&["show", "today", "--repo", "nope", "--no-scan"])
            .contains("No commits.")
    );

    let found = env.trail(&["search", "squash", "--no-scan"]);
    assert!(found.contains("  squash feat"), "{found}");
    assert_eq!(found.lines().count(), 3, "{found}");
    let found = env.trail(&[
        "search",
        "f2.txt",
        "--since",
        "today",
        "--json",
        "--no-scan",
    ]);
    let json: serde_json::Value = serde_json::from_str(&found).unwrap();
    assert_eq!(
        json.as_array().unwrap().len(),
        2,
        "f2 and the squash commit: {found}"
    );

    let repos = env.trail(&["repos"]);
    assert!(repos.starts_with("9  "), "{repos}");
    assert!(repos.trim_end().ends_with("app"), "{repos}");
}

#[test]
fn backfill_recovers_commits_the_reflog_lost() {
    let env = Env::new("backfill");
    env.git(&["init", "-q", "-b", "main"]);
    env.commit("mine");
    fs::write(env.repo().join("theirs.txt"), "theirs").unwrap();
    env.git(&["add", "."]);
    env.git(&[
        "commit",
        "-qm",
        "theirs",
        "--author=Other <other@example.com>",
    ]);
    fs::write(env.repo().join("alt.txt"), "alt").unwrap();
    env.git(&["add", "."]);
    env.git(&["commit", "-qm", "alt", "--author=BK <alt@example.com>"]);
    // Reflog expired: scan alone finds nothing.
    fs::remove_dir_all(env.repo().join(".git/logs")).unwrap();
    assert_eq!(env.trail(&["scan"]).trim(), "1 repos, 0 new commits");

    let args = [
        "backfill",
        "--author",
        "bk@example.com",
        "--author",
        "ALT@example.com",
    ];
    assert_eq!(env.trail(&args).trim(), "1 repos, 2 new commits");
    assert_eq!(env.trail(&args).trim(), "1 repos, 0 new commits");
    let today = env.trail(&["show", "--no-scan"]);
    assert!(
        today.contains("  mine") && today.contains("  alt"),
        "{today}"
    );
    assert!(!today.contains("theirs"), "{today}");
}
