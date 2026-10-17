import {
  Asterisk, Bot, Braces, Eye, Plug, SquareCode, SquareTerminal,
  type LucideIcon,
} from "lucide-react";
import type { ProviderKind, Session } from "../types";

const PROVIDER_ICON: Record<ProviderKind, LucideIcon> = {
  codex: SquareCode,
  claude: Asterisk,
  acp: Plug,
  opencode: Braces,
  pty: SquareTerminal,
  other: Bot,
};

export const PROVIDER_LABEL: Record<ProviderKind, string> = {
  codex: "Codex",
  claude: "Claude",
  acp: "ACP agent",
  opencode: "OpenCode",
  pty: "Terminal",
  other: "Agent",
};

export function ProviderAvatar({ s, size = "md" }: { s: Pick<Session, "provider" | "ownership">; size?: "sm" | "md" }) {
  const Icon = s.ownership === "observed" ? Eye : PROVIDER_ICON[s.provider] ?? Bot;
  return (
    <span className={`avatar ${size === "sm" ? "sm" : ""}`}>
      <Icon size={size === "sm" ? 14 : 17} strokeWidth={1.9} />
    </span>
  );
}

export const ACTIVE_STATES = ["starting", "running", "waiting_for_input", "waiting_for_approval"];

export function dotClass(state: string): string {
  if (state === "running") return "running";
  if (state === "starting") return "starting";
  if (state.startsWith("waiting")) return "waiting";
  if (state === "failed" || state === "unknown") return "failed";
  return "";
}

export function StateDot({ state }: { state: string }) {
  return <span className={`dot ${dotClass(state)}`} />;
}

const STATE_LABEL: Record<string, string> = {
  starting: "Starting",
  idle: "Idle",
  running: "Running",
  waiting_for_input: "Needs input",
  waiting_for_approval: "Needs approval",
  completed: "Completed",
  failed: "Failed",
  cancelled: "Cancelled",
  unknown: "Unknown",
};

export function stateLabel(state: string): string {
  return STATE_LABEL[state] ?? state.replaceAll("_", " ");
}

export function StateChip({ state }: { state: string }) {
  const tone = state.startsWith("waiting") ? "warn" : state === "failed" || state === "unknown" ? "bad" : "";
  return (
    <span className={`chip ${tone}`}>
      <StateDot state={state} />
      {stateLabel(state)}
    </span>
  );
}

export function ago(iso: string): string {
  const secs = Math.max(0, (Date.now() - Date.parse(iso)) / 1000);
  if (secs < 45) return "just now";
  if (secs < 3600) return `${Math.round(secs / 60)}m ago`;
  if (secs < 86400) return `${Math.round(secs / 3600)}h ago`;
  return new Date(iso).toLocaleDateString();
}

export function fmtElapsed(startedIso: string): string {
  const secs = Math.max(0, (Date.now() - Date.parse(startedIso)) / 1000);
  if (secs < 60) return `${Math.floor(secs)}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

export function fmtTokens(u: { input_tokens: number | null; output_tokens: number | null; source: string }): string | null {
  if (u.input_tokens == null && u.output_tokens == null) return null;
  const n = (u.input_tokens ?? 0) + (u.output_tokens ?? 0);
  const v = n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1000 ? `${(n / 1000).toFixed(1)}k` : `${n}`;
  return `${u.source === "provider_reported" ? "" : "≈"}${v} tokens`;
}
