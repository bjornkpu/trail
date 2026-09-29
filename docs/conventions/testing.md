# Testing

TDD, one behaviour at a time: write the test, watch it fail for the right reason, write the
least code that passes, refactor while green. Every non-trivial branch, parser, or state
transition leaves a test behind. If a parser or decision changed and no test changed,
something is missing.

Run with `cargo nextest run`, never `cargo test`. trail has no cargo features, so
`--all-features` changes nothing.

## Layers

- **Pure unit tests**, in-module (`#[cfg(test)] mod tests`) in `src/domain/`. Most tests live
  here: the reflog parser, the op filter, the session parser, dedupe, range parsing, paths.
  Plain data in, assert on plain data out.
- **Plan tests**: for decisions that return steps, assert the exact step sequence for each
  branch.
- **Store tests**, in `src/io/store.rs`: a real SQLite file in a temp dir.
- **Text snapshots**: `insta` for output a user reads (tables, help, rendered text). Inline
  (`@"..."`) when short, files when long.
- **Integration tests** in `tests/scan.rs`: run the real binary through
  `env!("CARGO_BIN_EXE_trail")`, with `TRAIL_HOME` and `CLAUDE_CONFIG_DIR` in a temp dir, and
  real `git`. One test covers commit, amend, branch, rebase, squash merge and a deleted
  branch: every squashed-away commit must be stored, and a second scan adds nothing. Another
  covers Claude sessions.

## Snapshots

Review with `cargo insta pending-snapshots` or `cargo insta review`. Accept only after
reading the new content. Never accept a snapshot you have not read, and never accept one to
make a red test green without understanding the diff.

## Integration test isolation

Nothing in a test may touch the user's real config, repos, or home. Point the tool's home at a
temp dir. trail has no `tempfile` dependency: tests use a dir under `std::env::temp_dir()`
named after the test and the process id, and remove it on drop. When a test runs git, isolate
it from the user's git config so hooks and signing never run:

```rust
fn command(&self, program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.current_dir(self.repo())
        .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "BK")
        .env("GIT_AUTHOR_EMAIL", "bk@example.com")
        .env("GIT_COMMITTER_NAME", "BK")
        .env("GIT_COMMITTER_EMAIL", "bk@example.com")
        .env("TRAIL_HOME", self.root.join("trail"))
        .env("CLAUDE_CONFIG_DIR", self.root.join("claude"));
    cmd
}
```

Start every file in `tests/` with `#![cfg(test)]`, so the `clippy.toml` test allowances
(`unwrap`, `expect`, `panic`, indexing) apply to its helpers too.

## What never runs in a test

Network calls, GPUs, model files, paid APIs, or external CLIs whose output is not
deterministic (`gh`, `az`, an LLM). Their argument building and reply parsing are pure and
tested with plain strings; the call itself sits behind a boundary trait with a fake (see
`architecture.md`). `git` is local and deterministic, so tests run it for real. The Windows
scheduler (`schtasks`) is never run in a test; the task XML it gets is built and tested as a
string.

## Later, when a bug motivates it

- `proptest` for parsers and state machines, once a bug shows example tests missed a case.
  It is not in `Cargo.toml`; ask BK before adding it.
- `cargo mutants` as a manual check for code no test would notice changing. Not in CI; it is
  slow.
