# Style

## Idioms

Modern idiomatic Rust, edition 2024:

- `let ... else` over nested `match`.
- Let chains (`if let Some(x) = a && x > 3`).
- Iterators and closures over index loops.
- `?` over manual matching on `Result`.
- `impl Trait` in argument position.
- `TryFrom` / `try_into` for narrowing numbers.
- `std::sync::LazyLock` over `lazy_static` / `once_cell`.
- `#[must_use]` on pure functions that return a value.
- `&str` and slices in parameters, owned types in struct fields.
- No `Rc<RefCell<_>>` in app state.
- No `.clone()` added only to satisfy the borrow checker without a comment saying why.

When unsure what is idiomatic, check the Rust API Guidelines and the current docs of the crate,
not old blog posts.

## Ponytail

Smallest working change. No speculative abstractions, no traits with one implementation
(outside the boundary rule in `architecture.md`), no config for values that never change, no
scaffolding for later. A deliberate shortcut with a known ceiling gets a `// ponytail:` comment
naming the ceiling and the upgrade path.

## Lints

`[lints.clippy]` in `Cargo.toml` is pedantic + nursery + panic-denying lints, all `deny`.
`clippy.toml` allows `unwrap`, `expect`, `panic` and indexing in tests; prototype there.

In practice:

- `.get()` not `[i]`
- `saturating_*` / `checked_*` not `+` / `-`
- `u16::try_from(..).unwrap_or(..)` not `as`
- `.chars().take(n)` not `&s[..n]`
- `saturating_duration_since` not `Instant - Instant`
- no `todo!()` scaffolding

`#[allow(clippy::...)]` needs a one-line comment saying why, on one item only, never
module-wide. Ask BK before relaxing `arithmetic_side_effects` or `as_conversions`.

## Comments

A comment says why, not what. Doc comments on items whose name does not already say it all.
Module docs (`//!`) say what the module owns and what it must not do.
