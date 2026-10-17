import { Check, CircleHelp, Minus, X } from "lucide-react";
import { useTranscript } from "../api";
import { useUi } from "../store";
import type { Capabilities, Session, Support } from "../types";
import { PROVIDER_LABEL, ProviderAvatar, StateChip, fmtElapsed } from "./ui";

const CAPS: [keyof Capabilities, string][] = [
  ["mid_turn_steer", "Steer"],
  ["cancel", "Cancel"],
  ["resume", "Resume"],
  ["fork", "Fork"],
  ["reconnect", "Reconnect"],
  ["streaming", "Streaming"],
  ["structured_approvals", "Approvals"],
  ["structured_questions", "Questions"],
  ["usage_reporting", "Usage"],
  ["artifact_reporting", "Artifacts"],
  ["list_sessions", "List"],
  ["import_session", "Import"],
];

const USAGE_SOURCE: Record<string, string> = {
  provider_reported: "Reported by provider",
  estimated: "Estimated",
  unavailable: "Unavailable",
};

export function Inspector({ s }: { s: Session }) {
  const { selectSession } = useUi();
  const transcript = useTranscript(s.id);

  return (
    <aside className="inspector">
      <div className="head">
        <ProviderAvatar s={s} />
        <div className="who">
          <div className="title">{s.title}</div>
          <div className="meta">{PROVIDER_LABEL[s.provider]} · {s.ownership}</div>
        </div>
        <button className="icon-btn" onClick={() => selectSession(null)} aria-label="Close inspector">
          <X size={15} />
        </button>
      </div>

      <div className="scroll">
        <div>
          <div style={{ marginBottom: 10 }}><StateChip state={s.state} /></div>
          <Kv k="Session" v={s.id} mono />
          {s.native_id && <Kv k="Native ID" v={s.native_id} mono />}
          {s.pid != null && <Kv k="PID" v={String(s.pid)} mono />}
          {s.activity_started_at && <Kv k="Active for" v={fmtElapsed(s.activity_started_at)} />}
          <Kv k="Updated" v={new Date(s.updated_at).toLocaleTimeString()} />
        </div>

        <div>
          <div className="label">Capabilities</div>
          <div className="caps">
            {CAPS.map(([key, label]) => <Cap key={key} label={label} v={s.capabilities[key]} />)}
          </div>
        </div>

        {s.usage && (
          <div>
            <div className="label">Usage <span style={{ fontWeight: 400 }}>· {USAGE_SOURCE[s.usage.source] ?? s.usage.source}</span></div>
            <div className="usage">
              <div className="stat"><div className="n">{fmtN(s.usage.input_tokens)}</div><div className="l">Input tokens</div></div>
              <div className="stat"><div className="n">{fmtN(s.usage.output_tokens)}</div><div className="l">Output tokens</div></div>
              {s.usage.cost_usd != null && (
                <div className="stat"><div className="n">${s.usage.cost_usd.toFixed(4)}</div><div className="l">Cost</div></div>
              )}
            </div>
          </div>
        )}

        <div>
          <div className="label">Output</div>
          <pre className="transcript">
            {transcript.isLoading ? "Loading…" : transcript.data?.text || "No output captured yet."}
          </pre>
        </div>
      </div>
    </aside>
  );
}

function Kv({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <div className="kv">
      <span className="k">{k}</span>
      <span className={`v selectable ${mono ? "mono" : ""}`} title={v}>{v}</span>
    </div>
  );
}

function Cap({ label, v }: { label: string; v: Support }) {
  const tip =
    v.status === "supported" ? v.constraint ?? "Supported"
    : v.status === "unsupported" ? v.reason
    : "Not yet negotiated with this provider";
  const cls = v.status === "supported" ? "" : v.status === "unsupported" ? "no" : "unknown";
  return (
    <div className={`cap ${cls}`} title={tip}>
      {v.status === "supported" ? <Check size={13} strokeWidth={2.2} />
        : v.status === "unsupported" ? <Minus size={13} />
        : <CircleHelp size={13} />}
      {label}
    </div>
  );
}

function fmtN(n: number | null) {
  return n == null ? "—" : n.toLocaleString();
}
