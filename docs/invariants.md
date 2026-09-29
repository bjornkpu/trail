# Invariants

Rules that must always hold. Each has an id, one sentence, and the test that pins it. Never
break one. An invariant without a test is a bug: write the test or delete the invariant.
Ids are never reused.

| Id | Invariant | Pinned by |
| --- | --- | --- |
| INV-1 | Every commit made locally is stored, including amended, rebased and squashed-away commits and commits on deleted branches. | `tests/scan.rs::scan_keeps_every_local_commit` |
| INV-2 | A second scan with no new reflog entries or prompts adds nothing. | `tests/scan.rs::scan_keeps_every_local_commit`, `tests/scan.rs::scan_records_claude_sessions` |
| INV-3 | Storage is append-only: recording the same data again adds no rows and removes none. | `src/io/store.rs::record_is_append_only_and_idempotent`, `src/io/store.rs::record_session_is_append_only_and_idempotent` |
| INV-4 | Only commit-creating reflog ops are kept; every other op is dropped. | `src/domain/reflog.rs::keeps_only_commit_creating_ops` |
| INV-5 | Subagent transcripts are never read. | `tests/scan.rs::scan_records_claude_sessions` |
| INV-6 | From a session only typed prompts and AskUserQuestion answers are stored, never Claude's replies or tool output. | `src/domain/claude.rs::keeps_typed_prompts_and_drops_everything_else`, `src/domain/claude.rs::answers_follow_question_order_with_notes` |
| INV-7 | Timestamps are stored as UTC ISO 8601. | `src/io/store.rs::timestamps_are_stored_as_utc_iso` |
| INV-8 | Logs go to `trail.log`, never stdout; `scan --quiet` writes nothing to stdout. | `tests/scan.rs::scan_keeps_every_local_commit` |
| INV-9 | The database runs in WAL mode, so a scheduled scan and an interactive command can overlap. | `src/io/store.rs::open_is_idempotent_and_uses_wal` |
| INV-10 | Versions of one commit (same author date, author email and subject, as after a rebase) show once, as the newest version. | `src/domain/view.rs::dedupe_keeps_newest_version` |
