# bridges/claude — optional Agent SDK bridge

The daemon's native Claude integration uses the documented headless CLI
(`claude --output-format stream-json --resume <session>`), which covers
streaming, resume, and cancellation.

For *full* approval/user-input handling (`canUseTool` callbacks), the
official **TypeScript** Agent SDK is required. Rather than pulling a JS
runtime into the daemon, Orc treats the SDK as an optional sidecar:

```
orcd ──(NDJSON over stdin/stdout, versioned)──► bridge (node)
                                                    │ official SDK
                                                    ▼
                                              Claude Agent SDK
```

- The bridge is a *client* of the SDK, never an authority. Sessions,
  policies, state, and truth stay in Rust.
- `bridge.mjs` here is the protocol skeleton: line-delimited JSON
  requests (`send`, `answer_tool`, `cancel`) → SDK calls; SDK events →
  Orc adapter events. It is intentionally small — the daemon-side
  adapter (`crates/adapters/src/claude.rs`, `with_bridge`) detects a
  configured bridge and upgrades itself.
- API keys: the bridge reads `ANTHROPIC_API_KEY` itself; Orc never
  copies subscription tokens between providers.

This directory ships the contract + skeleton, not a packaged service —
installing it is a deliberate opt-in.
