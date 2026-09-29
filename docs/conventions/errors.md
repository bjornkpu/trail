# Errors, logging, output

## Errors

- Everything below `main` returns `Result<_, AppError>`. `AppError` is one enum in
  `src/error.rs`, built with `thiserror`.
- `anyhow` only in `main.rs`, for context at the boundary (`.context("reading config")`).
- A variant's message says what went wrong and, when there is one, what to do about it:
  `"cannot find the home directory; set TRAIL_HOME or the XDG_*_HOME variables"`.
- Wrap foreign errors with `#[from]` when the conversion is lossless, or a variant with fields
  when the caller needs to know which file or which command failed.
- Never swallow an error. `let _ =` on a `Result` needs a comment saying why ignoring it is
  right (for example, the receiver is gone because the app already quit).
- Per-item failures in a batch (one repo of many, one file of many) log a warning and
  continue. Failures that make the result wrong (the database, the config) are fatal. In
  trail: a missing object or `git` not on `PATH` warns and moves to the next commit or repo,
  and the reflog entry is still stored. A database error ends the run.
- No panics in non-test code: the lints deny `unwrap`, `expect`, `panic`, indexing and
  unchecked arithmetic.

## Logging

- `tracing` macros everywhere; `tracing-subscriber` and `tracing-appender` set up once, in
  `init_logging` in `src/main.rs`.
- Logs go to `trail.log` in the state dir (see `architecture.md`), never stdout (INV-8).
  stderr is for errors the user must see.
- trail differs from the default here: logging is always on at `info`, with no env var to set
  the level. A scheduled scan runs headless, and the log is the only record of what it did.
- Log what a later debugging session needs: commands spawned, requests and statuses, files
  written, decisions taken. `warn!` on every failure that does not stop the run.

## Output

- stdout carries only what the command produces, so it can be piped. `scan --quiet` writes
  nothing to stdout.
- A `--json` output, when a tool has one, is stable: add fields, never rename or remove them.
- Errors print once, from `main`, through `anyhow`'s display.
