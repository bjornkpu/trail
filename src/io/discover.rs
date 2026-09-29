use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// A repository and every reflog file that records its commits.
#[derive(Debug)]
pub struct Repo {
    /// Main working tree, or the directory holding the `.git` file when the git dir is not
    /// named `.git` (submodules, worktrees of bare repos).
    pub path: PathBuf,
    /// Sorted by ref name.
    pub logs: Vec<RefLog>,
}

#[derive(Debug)]
pub struct RefLog {
    /// `HEAD`, `refs/heads/<branch>` or `worktrees/<name>/HEAD`.
    pub ref_name: String,
    pub path: PathBuf,
}

/// Walks `roots` up to `max_depth` levels down, skipping directories named in `skip`, and
/// returns each repository once, sorted by path. Worktrees resolve to their main repository.
/// Unreadable directories are skipped.
#[must_use]
pub fn discover(roots: &[PathBuf], max_depth: usize, skip: &[String]) -> Vec<Repo> {
    let mut found = BTreeMap::new();
    let mut stack: Vec<_> = roots.iter().map(|r| (normalize(r), 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        if let Some(common) = common_dir(&dir) {
            let path = if common.file_name().is_some_and(|n| n == ".git") {
                common
                    .parent()
                    .map_or_else(|| dir.clone(), Path::to_path_buf)
            } else {
                dir.clone()
            };
            found.entry(common).or_insert(path);
        }
        if depth >= max_depth {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            // file_type does not follow symlinks, so links and junctions are never walked.
            if entry.file_type().is_ok_and(|t| t.is_dir())
                && name != ".git"
                && !skip.iter().any(|s| name == s.as_str())
            {
                stack.push((entry.path(), depth.saturating_add(1)));
            }
        }
    }
    let mut repos: Vec<_> = found
        .into_iter()
        .map(|(common, path)| Repo {
            path,
            logs: reflogs(&common),
        })
        .collect();
    repos.sort_by(|a, b| a.path.cmp(&b.path));
    repos
}

/// The common git dir of the repository whose working tree is `dir`, if it has one.
fn common_dir(dir: &Path) -> Option<PathBuf> {
    let dot_git = dir.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    // Worktrees and submodules have a `.git` file: `gitdir: <path>`, maybe relative.
    let text = fs::read_to_string(&dot_git).ok()?;
    let git_dir = normalize(&dir.join(text.strip_prefix("gitdir:")?.trim()));
    // Worktree git dirs name their common dir in `commondir`; submodule git dirs are their own.
    Some(match fs::read_to_string(git_dir.join("commondir")) {
        Ok(common) => normalize(&git_dir.join(common.trim())),
        Err(_) => git_dir,
    })
}

/// Existing reflog files under a common git dir, sorted by ref name.
fn reflogs(common: &Path) -> Vec<RefLog> {
    let mut logs = Vec::new();
    let mut add = |ref_name: String, path: PathBuf| {
        if path.is_file() {
            logs.push(RefLog { ref_name, path });
        }
    };
    add("HEAD".into(), common.join("logs").join("HEAD"));
    let mut stack = vec![("refs/heads".to_owned(), common.join("logs/refs/heads"))];
    while let Some((prefix, dir)) = stack.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let ref_name = format!("{prefix}/{}", entry.file_name().to_string_lossy());
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push((ref_name, entry.path()));
            } else {
                add(ref_name, entry.path());
            }
        }
    }
    for entry in fs::read_dir(common.join("worktrees"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        add(
            format!("worktrees/{name}/HEAD"),
            entry.path().join("logs").join("HEAD"),
        );
    }
    logs.sort_by(|a, b| a.ref_name.cmp(&b.ref_name));
    logs
}

/// Resolves `.` and `..` without touching the filesystem. `canonicalize` would turn
/// Windows paths into `\\?\` form.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn refs(repo: &Repo) -> Vec<&str> {
        repo.logs.iter().map(|l| l.ref_name.as_str()).collect()
    }

    #[test]
    fn finds_repos_worktrees_and_submodules() {
        let root = std::env::temp_dir().join(format!("trail-discover-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let main = root.join("main");
        let git = main.join(".git");
        touch(&git.join("logs/HEAD"), "");
        touch(&git.join("logs/refs/heads/main"), "");
        touch(&git.join("logs/refs/heads/feat/x"), "");
        touch(&git.join("worktrees/wt/commondir"), "../..\n");
        touch(&git.join("worktrees/wt/logs/HEAD"), "");
        // A worktree, with the absolute gitdir git writes by default.
        let wt_gitdir = git.join("worktrees/wt");
        touch(
            &root.join("wts/wt/.git"),
            &format!("gitdir: {}\n", wt_gitdir.display()),
        );
        // A submodule: relative gitdir, no commondir.
        let sup = root.join("super");
        touch(&sup.join(".git/logs/HEAD"), "");
        touch(&sup.join(".git/modules/sub/logs/HEAD"), "");
        touch(&sup.join("sub/.git"), "gitdir: ../.git/modules/sub\n");
        // Pruned by name, and too deep.
        touch(&root.join("node_modules/dep/.git/logs/HEAD"), "");
        touch(&root.join("a/b/c/deep/.git/logs/HEAD"), "");

        let repos = discover(std::slice::from_ref(&root), 3, &["node_modules".into()]);

        let paths: Vec<_> = repos.iter().map(|r| r.path.clone()).collect();
        assert_eq!(paths, [main, sup.clone(), sup.join("sub")]);
        assert_eq!(
            refs(&repos[0]),
            [
                "HEAD",
                "refs/heads/feat/x",
                "refs/heads/main",
                "worktrees/wt/HEAD"
            ]
        );
        assert_eq!(repos[0].logs[3].path, wt_gitdir.join("logs").join("HEAD"));
        assert_eq!(refs(&repos[1]), ["HEAD"]);
        assert_eq!(refs(&repos[2]), ["HEAD"]);
        assert_eq!(
            repos[2].logs[0].path,
            sup.join(".git/modules/sub/logs/HEAD")
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_root_finds_nothing() {
        let root = std::env::temp_dir().join("trail-discover-does-not-exist");
        assert!(discover(&[root], 4, &[]).is_empty());
    }

    #[test]
    fn root_spelling_does_not_change_repo_path() {
        let root = std::env::temp_dir().join(format!("trail-discover-sep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        touch(&root.join("r/.git/logs/HEAD"), "");
        let slashed = PathBuf::from(root.display().to_string().replace('\\', "/"));
        let a = discover(std::slice::from_ref(&root), 2, &[]);
        let b = discover(&[slashed], 2, &[]);
        assert_eq!(
            a[0].path.display().to_string(),
            b[0].path.display().to_string()
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn normalize_resolves_dot_dot() {
        assert_eq!(
            normalize(Path::new("/r/.git/worktrees/wt/../..")),
            PathBuf::from("/r/.git")
        );
    }
}
