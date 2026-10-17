# Orc

A local AI session manager with a conversational front end — not a chatbot that happens to run shell commands.

**The model interprets requests and proposes actions. The Rust runtime owns sessions, permissions, execution, and the truth about what is happening.**

```
React interface (chat / live / attention inbox)
      │
Tauri 2 bridge — authenticated commands + event channel only
      │        (the webview never holds daemon credentials)
      ▼
orcd — standalone Rust orchestration daemon
  command service  · session controllers · task scheduler
  policy engine    · SQLite event log + projections
  provider adapters ── Codex · ACP · Claude · OpenCode · PTY
      ▲
      └── orc CLI (same IPC surface — headless control)
```

Two executables, two lifecycles: **closing the window never kills an agent.** The desktop app auto-starts `orcd` when needed, but "close window", "quit interface", and "stop all work" are different actions (tray menu). The daemon also runs happily on its own — the CLI talks to it over the same local socket.

## Experiences

- **Chat** — the control surface. "What is running?", "resume the auth session", "stop the review but leave the implementation" produce real runtime operations — `sessions.start`, `sessions.cancel`, … — via the same command path as buttons and the CLI. Sessions started from chat render as cards fed by the runtime, never by the model's prose.
- **Live** — work by workspace and task. Session cards show state, latest meaningful update, pending questions, elapsed time, and controls. Facts ("waiting for approval" — a protocol event) and inferences ("may be stalled" — labeled as an inference) render differently.
- **Attention inbox** — approvals, questions, failed auth, review requests, uncertain outcomes. Same underlying items as chat; answering in either place updates both.

Ownership is first-class: `managed` (Orc launched it), `imported` (native conversation pulled in — controls depend on adapter capabilities), `observed` (a process we detected — visibly not controllable).

## Safety model

- **Intent before effect.** Every command is persisted with an idempotency key *before* dispatch; a crash between dispatch and ack reconciles to `outcome_unknown`, never a blind retry.
- **Bound approvals.** An approval stores the exact action + args hash; it cannot authorize a changed command later. Ordinary answers are a different type from permission grants, and nothing gives the model blanket approval.
- **Dead providers die honestly.** When a session's provider exits, its open approvals/questions are invalidated (`expires_with_session`) so buttons can't reach a dead request.
- **Private interface.** Unix socket / named pipe with a 0600-token handshake and protocol-version negotiation. The webview talks only through the Tauri bridge.
- **Facts vs. inferences** are separate types end-to-end. "No recent output" never renders as failure.

## Repo layout

```
apps/desktop/          React + TS + Vite frontend; src-tauri/ Tauri 2 bridge
crates/
  domain/              entities, commands, events, capabilities (schema v1, ts-rs exportable)
  storage/             SQLite (WAL): durable event log + projections, transcripts, migrations
  runtime/             session controllers (bounded channels), scheduler, policy, reconcile
  adapters/            capability-based adapters: codex, acp, claude, opencode, pty, observed, discover
  orchestrator/        ModelBackend trait + typed tool surface + chat loop
  daemon/              orcd — local IPC server, observer, reaper
  cli/                 orc — headless control + doctor diagnostics
bridges/claude/        optional TypeScript Agent SDK bridge (contract + skeleton)
tests/
  fixtures/            redacted recorded provider streams (replay tests)
  fake-providers/      orc-fake-agent — lifecycle/failure injection binary
```

## Adapters are capabilities, not names

`AgentAdapter` exposes a session with an explicit capability matrix — `supported` / `unsupported{reason}` / `unknown` — per operation (send, steer, cancel, resume, fork, stream, structured approvals, structured questions, usage, artifacts). The PTY fallback supports terminal interaction and output capture; a model-inferred "question" from raw text is labeled `inferred_prompt` and never silently becomes an approval.

| Integration | Path | Notes |
|---|---|---|
| Codex | App Server JSON-RPC over stdio | pinned, recorded fixtures |
| ACP agents | agent-client-protocol Rust SDK (stable v1) | negotiated capabilities |
| Claude | documented headless CLI stream-json | optional SDK bridge for `canUseTool` |
| OpenCode | local HTTP API + SSE (explicit config) | bound + authenticated |
| Anything else | structured stdout → `portable-pty` | visibly limited surface |

## The chat orchestrator is a restricted client

Its entire reach is a typed tool surface over the runtime's command service: `tools.list`, `workspaces.list`, `sessions.{list,inspect,start,send,cancel,resume}`, `attention.{list,answer_question}`, `tasks.create`, `artifacts.read`. Model config is env (`ORC_MODEL_API_KEY`, `ORC_MODEL`, `ORC_MODEL_BACKEND=mock` for offline dev) — *not* borrowed from worker agents' subscriptions. The UI works fully when inference is unavailable.

## Persistence

SQLite + WAL. Lifecycle events are durable immediately; text deltas batch (150 ms / 32 KB flush) into rotated transcript files, not per-token transactions. Every event carries an app-owned id, seq, session/run refs, source, timestamp, and schema version — the UI loads a snapshot then follows the event stream with replay-on-reconnect.

## Development

```bash
cargo build --workspace          # orcd + orc + libs
cargo test  --workspace          # lifecycle, policy, store, IPC, replay tests
pnpm --dir apps/desktop install  # frontend
pnpm --dir apps/desktop dev      # UI in a browser (demo transport)
pnpm --dir apps/desktop tauri dev

# headless
./target/debug/orcd --runtime-dir ~/.orc --mock-model
./target/debug/orc doctor
./target/debug/orc session start --tool <id> --workspace <id> --title t --prompt "…"
```

## License

Apache-2.0
