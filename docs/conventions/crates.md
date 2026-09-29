# Crates

The chosen crate for each job, and why. This file is the authority on dependencies. Never add
an alternative for a job a listed crate does. Ask BK before adding anything not listed.

Versions are pinned to the major only (`"1"`, `"0.40"`); `Cargo.lock` holds the exact version.

## In use

| Job | Crate | Why |
| --- | --- | --- |
| Error type | `thiserror` | One `AppError` enum with derived messages and `#[from]`. |
| Error context in `main` | `anyhow` | `.context()` at the boundary; never below `main`. |
| Command line | `clap` (`derive`) | Derive keeps arguments, help and parsing in one struct. |
| Logging | `tracing`, `tracing-appender`, `tracing-subscriber` | Structured logs to `trail.log` in the state dir. |
| (De)serialisation | `serde` (`derive`), `serde_json`, `toml` | The standard; `toml` for config, JSON for Claude session files and `--json` output. |
| Dates and times | `jiff` | Correct time zones and spans, a clear API. |
| SQLite | `rusqlite` (`bundled`) | The default for a sync CLI. Bundled, so no system SQLite needed. |
| Snapshot tests (dev) | `insta` | Output a user reads, reviewed as text. |

Temp dirs in tests come from `std::env::temp_dir()`, so there is no `tempfile`.

## Rejected

| Crate | Instead | Why |
| --- | --- | --- |
| `chrono` | `jiff` | jiff handles time zones and spans correctly with a smaller API surface. |
| `once_cell`, `lazy_static` | `std::sync::LazyLock` | In std since 1.80. |
| `color-eyre` | `anyhow` | Pretty reports add little in a CLI. |
| `git2` | spawn `git` | libgit2 lags git and needs native builds. `gix` only if spawning becomes a measured problem. Banned in `deny.toml`. |
| `openssl` | rustls | No system OpenSSL to install or patch. `openssl-sys` banned in `deny.toml`. |
| `assert_cmd` | `std::process::Command` + `env!("CARGO_BIN_EXE_trail")` | std covers it. |

## Adding a license

`deny.toml` allows permissive licenses (MIT, Apache-2.0, BSD, BSL-1.0, CDLA-Permissive-2.0,
ISC, Unicode-3.0, Unlicense, Zlib) and MPL-2.0, which only asks that changes to MPL files stay open. The
GPL family stays out: it would make the whole binary GPL. When a chosen crate needs a license
not on the list, add it with a comment naming that crate. A crate with no license gets a
`[[licenses.clarify]]` or `exceptions` entry with a reason, or is replaced. Never add a
license to unblock a crate that is not chosen.
