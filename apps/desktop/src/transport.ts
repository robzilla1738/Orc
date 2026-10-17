// Transport: inside Tauri the frontend goes through the Rust bridge
// (invoke + Channel); in a plain browser during dev it falls back to a
// clearly-labelled demo transport with fabricated data so the UI is
// still inspectable.

import type {
  ChatReply, ProviderKind, ServerEvent, Session, Snapshot,
} from "./types";
import { UNKNOWN_CAPABILITIES } from "./types";

export interface Transport {
  request(method: string, params?: unknown): Promise<unknown>;
  subscribe(onEvent: (e: ServerEvent) => void, onReset: () => void): Promise<() => void>;
  readonly label: string;
}

declare global {
  interface Window {
    __TAURI__?: unknown;
  }
}

export function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI__" in window;
}

class TauriTransport implements Transport {
  label = "daemon";
  private ready: Promise<typeof import("@tauri-apps/api/core")> | null = null;
  private load() {
    if (!this.ready) {
      this.ready = import("@tauri-apps/api/core");
    }
    return this.ready;
  }
  async request(method: string, params?: unknown) {
    const { invoke } = await this.load();
    const res = (await invoke("orc_request", {
      method,
      params: params ?? {},
    })) as { result?: unknown; error?: string };
    if (res.error) throw new Error(res.error);
    return res.result;
  }
  async subscribe(onEvent: (e: ServerEvent) => void, onReset: () => void) {
    const { invoke, Channel } = await this.load();
    const chan = new Channel<ServerEvent | { type: "resync_required" }>();
    chan.onmessage = (msg: ServerEvent | { type: "resync_required" }) => {
      if ((msg as { type?: string }).type === "resync_required") onReset();
      else onEvent(msg as ServerEvent);
    };
    await invoke("orc_events_subscribe", { channel: chan });
    return () => {
      invoke("orc_events_unsubscribe").catch(() => {});
    };
  }
}

// ---- Demo transport (browser dev only) --------------------------------

const demoSession = (id: string, title: string, state: Session["state"], provider: ProviderKind, extra: Partial<Session> = {}): Session => ({
  id,
  workspace_id: "ws_demo",
  task_id: null,
  tool_instance_id: "tool_pty",
  provider,
  ownership: "managed",
  native_id: null,
  title,
  state,
  status_detail: null,
  capabilities: {
    ...UNKNOWN_CAPABILITIES,
    send: { type: "supported" },
    cancel: { type: "supported" },
    resume: { type: "supported" },
  },
  pid: 1234,
  last_meaningful_update: "wrote crates/runtime/src/controller.rs",
  usage: null,
  last_event_seq: 0,
  created_at: new Date(Date.now() - 8 * 60000).toISOString(),
  updated_at: new Date().toISOString(),
  activity_started_at: new Date(Date.now() - 3 * 60000).toISOString(),
  ...extra,
});

class DemoTransport implements Transport {
  label = "demo";
  private snap: Snapshot = {
    cursor: 4,
    sessions: [
      demoSession("ses_a1", "Refactor session controller", "running", "codex", {
        status_detail: { kind: "inference", message: "may be stalled — no output for 6m" },
      }),
      demoSession("ses_b2", "Review auth flow", "waiting_for_approval", "claude", {
        status_detail: { kind: "fact", message: "provider requested permission" },
      }),
      demoSession("ses_c3", "Port settings page", "idle", "opencode"),
    ],
    runs: [],
    attention: [
      {
        id: "att_1",
        session_id: "ses_b2",
        run_id: "run_1",
        workspace_id: "ws_demo",
        kind: "approval",
        status: "open",
        title: "claude wants to run a command",
        body: "git push --force-with-lease origin feature/auth",
        pending_action: {
          kind: "exec",
          display: "git push --force-with-lease origin feature/auth",
          args: { cmd: "git push --force-with-lease origin feature/auth" },
          args_hash: "ab12…",
          provider_ref: "req_42",
        },
        choices: [],
        provider_request_ref: "req_42",
        expires_at: null,
        expires_with_session: true,
        created_at: new Date().toISOString(),
      },
    ],
    tasks: [],
    tools: [
      { id: "tool_codex", provider: "codex", label: "Codex CLI", executable: "codex", version: "0.42.0", readiness: { type: "ready" }, settings: {} },
      { id: "tool_claude", provider: "claude", label: "Claude Code", executable: "claude", version: "2.0.0", readiness: { type: "installed_unauthenticated" }, settings: {} },
      { id: "tool_pty", provider: "pty", label: "Terminal", executable: "bash", version: null, readiness: { type: "ready" }, settings: {} },
    ],
    workspaces: [{ id: "ws_demo", name: "Orc", root: "~/repos/Orc", allow_rules: [], network_allowed: false, remote_content_allowed: true }],
    pending_commands: [],
  };
  private seq = 4;
  private chat: { id: string; role: string; content: any; operations: string[]; at: string }[] = [
    { id: "m1", role: "assistant", content: { text: "What would you like to work on?" }, operations: [], at: new Date().toISOString() },
  ];

  async request(method: string, params?: any): Promise<unknown> {
    await new Promise((r) => setTimeout(r, 60));
    switch (method) {
      case "snapshot.get":
        return this.snap;
      case "chat.history":
        return this.chat;
      case "chat.send": {
        const text = String(params?.text ?? "");
        this.chat.push({ id: `m${this.seq++}`, role: "user", content: { text }, operations: [], at: new Date().toISOString() });
        const reply: ChatReply = {
          message_id: `m${this.seq++}`,
          text: `Demo mode — connect \`orcd\` to make this real. You said: “${text}”.`,
          operations: [],
          offline: true,
        };
        this.chat.push({ id: reply.message_id, role: "assistant", content: { text: reply.text }, operations: [], at: new Date().toISOString() });
        return reply;
      }
      case "command.exec":
        return { type: "applied", result: {} };
      case "diagnostics":
        return { version: "demo", protocol: 1, sessions: this.snap.sessions.length, tools: 3, workspaces: 1, open_attention: 1, pending_intents: 0 };
      case "transcript.tail":
        return { text: "… demo transcript …", cursor: 0 };
      case "discover.run":
        return { tools: [] };
      case "ping":
        return { pong: true };
      default:
        return {};
    }
  }
  async subscribe(_on: (e: ServerEvent) => void, _reset: () => void) {
    return () => {};
  }
}

export const transport: Transport = inTauri() ? new TauriTransport() : new DemoTransport();
