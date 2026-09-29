# Architecture

## Pure core, thin IO shell

The shell gathers facts, a pure function decides, the shell executes. Facts in, plan out.

- `src/domain/` is pure. It takes and returns plain data: no filesystem, no network, no
  processes, no clock, no environment variables. Anything it needs from the world arrives as
  a parameter (`paths::resolve` takes the home dir and an env lookup; `range::parse` takes
  today's date).
- `src/io/` does all IO: reading files, spawning `git`, SQLite, the scheduler, the terminal.
- `src/main.rs` is wiring: parse arguments, start logging, gather facts through `io`, call
  `domain`, execute the result through `io`. `anyhow` lives here and nowhere else.

Because `domain` is plain data, it is tested directly with no mocks and no fakes. When a
decision is hard to test, the IO has leaked into it: move the IO out and pass its result in.

A decision with several effects returns them as data (a `Vec<Step>`, a plan struct) for `io`
to execute. Tests assert on the plan; one integration test proves the executor runs it.

## Traits: the boundary rule

No traits with one implementation, with one exception: a boundary trait per external service
that is slow, costly, or non-deterministic (a network API, a GPU model, an LLM). It has the
real implementation and a fake, and the fake is the point: tests never touch the service.

```rust
pub trait Forecast {
    async fn today(&self, city: &str) -> Result<Weather, AppError>;
}

pub struct HttpForecast { client: reqwest::Client, base: String }
impl Forecast for HttpForecast { /* real call */ }

#[cfg(test)]
pub struct FakeForecast(pub Weather);
#[cfg(test)]
impl Forecast for FakeForecast {
    async fn today(&self, _city: &str) -> Result<Weather, AppError> { Ok(self.0.clone()) }
}
```

Local things are not boundaries. git, SQLite and the filesystem are used for real in tests,
inside a temp dir (see `testing.md`). trail talks only to local things, so it has no boundary
trait.

## Typestate

When a value moves through states that must never be mixed at runtime (unvalidated then
validated, draft then sent), make each state its own type and make the transition a function
that consumes one and returns the next. The compiler then rejects the mix-up.

## Paths

trail differs from the default single home dir. It follows XDG, resolved in
`src/domain/paths.rs`:

| What   | Default                          | Override          |
|--------|----------------------------------|-------------------|
| Config | `~/.config/trail/config.toml`    | `XDG_CONFIG_HOME` |
| DB     | `~/.local/share/trail/trail.db`  | `XDG_DATA_HOME`   |
| Log    | `~/.local/state/trail/trail.log` | `XDG_STATE_HOME`  |

`TRAIL_HOME` overrides everything and puts all three in one dir. Empty or relative `XDG_*`
values are ignored. Claude Code's dir is `CLAUDE_CONFIG_DIR`, else `~/.claude`.

## Storage

trail keeps its data in SQLite through `rusqlite` (bundled), all of it in `src/io/store.rs`.
The schema and the storage rules are in `CLAUDE.md`.
