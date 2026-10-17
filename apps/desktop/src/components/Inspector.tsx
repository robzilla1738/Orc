import { useTranscript } from "../api";
import { useUi } from "../store";
import type { Session, Snapshot } from "../types";
import { fmtElapsed } from "./LiveView";

export function Inspector({ snap }: { snap: Snapshot }) {
  const { selectedSession, inspectorOpen, toggleInspector } = useUi();
  const session = snap.sessions.find((s) => s.id === selectedSession) ?? null;
  const transcript = useTranscript(inspectorOpen ? selectedSession : null);

  if (!inspectorOpen) {
    return (
      <div style={{ padding: 12 }}>
        <button className="pill ghost small" onClick={toggleInspector} title="Show inspector">◂</button>
      </div>
    );
  }

  return (
    <div className="inspector">
      <div style={{ display: "flex", alignItems: "center" }}>
        <div className="sec-label" style={{ flex: 1 }}>Inspector</div>
        <button className="pill ghost small" onClick={toggleInspector}>▸</button>
      </div>
      {!session ? (
        <div style={{ color: "var(--ink-3)", fontSize: "var(--fs-s)" }}>
          Select a session to see its process, usage, and transcript.
        </div>
      ) : (
        <Details s={session} transcript={transcript.data?.text ?? ""} loading={transcript.isLoading} />
      )}
    </div>
  );
}

function Details({ s, transcript, loading }: { s: Session; transcript: string; loading: boolean }) {
  return (
    <>
      <div>
        <div style={{ fontWeight: 600, marginBottom: 6 }}>{s.title}</div>
        <Kv k="state" v={s.state.replaceAll("_", " ")} />
        <Kv k="provider" v={`${s.provider} · ${s.ownership}`} />
        <Kv k="session" v={s.id} />
        {s.native_id && <Kv k="native id" v={s.native_id} />}
        {s.pid != null && <Kv k="pid" v={String(s.pid)} />}
        {s.activity_started_at && <Kv k="active for" v={fmtElapsed(s.activity_started_at)} />}
        <Kv k="updated" v={new Date(s.updated_at).toLocaleTimeString()} />
      </div>

      <div>
        <div className="sec-label">Capabilities</div>
        <Cap k="send" v={s.capabilities.send} />
        <Cap k="steer" v={s.capabilities.steer} />
        <Cap k="cancel" v={s.capabilities.cancel} />
        <Cap k="resume" v={s.capabilities.resume} />
        <Cap k="approvals" v={s.capabilities.structured_approval} />
        <Cap k="questions" v={s.capabilities.structured_question} />
        <Cap k="usage" v={s.capabilities.usage_report} />
      </div>

      {s.usage && (
        <div>
          <div className="sec-label">Usage ({s.usage.source})</div>
          <Kv k="input" v={fmtN(s.usage.input_tokens)} />
          <Kv k="output" v={fmtN(s.usage.output_tokens)} />
          {s.usage.cost_usd != null && <Kv k="cost" v={`$${s.usage.cost_usd.toFixed(4)}`} />}
        </div>
      )}

      <div style={{ flex: 1, minHeight: 0, display: "flex", flexDirection: "column", gap: 6 }}>
        <div className="sec-label">Transcript tail</div>
        <pre>{loading ? "loading…" : transcript || "—"}</pre>
      </div>
    </>
  );
}

function Kv({ k, v }: { k: string; v: string }) {
  return (
    <div className="kv"><span className="k">{k}</span><span className="v">{v}</span></div>
  );
}

function Cap({ k, v }: { k: string; v: { type: string; reason?: string; constraint?: string } }) {
  const label = v.type === "supported" ? "yes" : v.type === "unsupported" ? "no" : "?";
  const color = v.type === "supported" ? "var(--good)" : v.type === "unsupported" ? "var(--ink-3)" : "var(--warn)";
  return (
    <div className="kv">
      <span className="k">{k}</span>
      <span className="v" style={{ color }}>{label}{v.constraint ? ` · ${v.constraint}` : ""}</span>
    </div>
  );
}

function fmtN(n: number | null) {
  return n == null ? "—" : n.toLocaleString();
}
