#!/usr/bin/env node
// orc claude bridge — protocol skeleton (see README.md).
// NDJSON over stdio: one JSON request per line from orcd, one JSON
// event per line back. Wire this to @anthropic-ai/claude-agent-sdk's
// query() where marked below.

const readline = require("node:readline");
const rl = readline.createInterface({ input: process.stdin });
const send = (obj) => process.stdout.write(JSON.stringify(obj) + "\n");

send({ type: "bridge.hello", protocol: 1, sdk: "claude-agent-sdk" });

let current = null; // { abort: AbortController }

rl.on("line", async (line) => {
  let req;
  try { req = JSON.parse(line); } catch { return; }
  switch (req.type) {
    case "send": {
      // const { query } = await import("@anthropic-ai/claude-agent-sdk");
      // current = { abort: new AbortController() };
      // for await (const msg of query({ prompt: req.text, options: {
      //   resume: req.native_id,
      //   canUseTool: (tool, input) => new Promise((resolve) =>
      //     pendingApprovals.set(input.id, resolve) &&
      //     send({ type: "approval_request", id: input.id, tool, input })
      //   ),
      //   abortController: current.abort,
      // }})) send({ type: "sdk.message", message: msg });
      send({ type: "error", reason: "sdk not installed" });
      break;
    }
    case "answer_tool": {
      // pendingApprovals.get(req.id)?.({ behavior: req.approve ? "allow" : "deny", message: req.message });
      break;
    }
    case "cancel": {
      current?.abort.abort();
      break;
    }
  }
});
