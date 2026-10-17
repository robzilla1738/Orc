# tests/fixtures — recorded provider streams

Redacted, recorded provider outputs for replay tests. Each file is
newline-delimited JSON matching the provider's documented output format,
with credentials, file paths, and user content stripped.

- `claude_stream_basic.jsonl` — a normal `claude -p` stream-json turn:
  init → assistant text → result with usage.
- `claude_stream_approval.jsonl` — a turn that raises a permission
  request then ends (the daemon must leave an open approval item when
  the provider dies).
- `pty_hang_no_output.bin` — a pty capture with output followed by
  silence; used for stall-inference tests (a stall is *labeled as an
  inference*, never a fact).

Replay principle: fixtures feed the *adapter's* parser, not the wire —
the runtime only ever sees adapter events, so replay tests exercise
exactly the parsing/state path production uses.
