import { Activity, ArrowUp, ArrowUpRight, Play, Square, Wand2 } from "lucide-react";
import { useState } from "react";
import { cancelSession, resumeSession, sendSessionMessage, useCommand } from "../api";
import { useUi } from "../store";
import { isSupported, type Session, type Snapshot } from "../types";
import { ACTIVE_STATES, PROVIDER_LABEL, ProviderAvatar, StateChip, ago, fmtElapsed, fmtTokens } from "./ui";

export function LiveView({ snap }: { snap: Snapshot }) {
  const byWorkspace = new Map<string, Session[]>();
  for (const s of snap.sessions) {
    const list = byWorkspace.get(s.workspace_id) ?? [];
    list.push(s);
    byWorkspace.set(s.workspace_id, list);
  }
  const active = snap.sessions.filter((s) => ACTIVE_STATES.includes(s.state)).length;

  return (
    <>
      <div className="topbar drag" data-tauri-drag-region>
        <h1>Live</h1>
        <span className="sub">
          {snap.sessions.length} session{snap.sessions.length === 1 ? "" : "s"} · {active} active
        </span>
      </div>
      <div className="scroll live">
        {snap.sessions.length === 0 && (
          <div className="empty">
            <span className="avatar"><Activity size={18} /></span>
            <div className="title">Nothing running</div>
            <div className="body">Sessions you start from chat or the CLI appear here, grouped by workspace.</div>
          </div>
        )}
        {[...byWorkspace.entries()].map(([wid, sessions]) => {
          const ws = snap.workspaces.find((w) => w.id === wid);
          return (
            <section key={wid}>
              <div className="ws-head">
                <span className="name">{ws?.name ?? "Workspace"}</span>
                {ws?.root && <span className="root mono">{ws.root}</span>}
              </div>
              <div className="grid">
                {sessions.map((s) => (
                  <SessionCard
                    key={s.id}
                    s={s}
                    openAttention={snap.attention.filter((a) => a.session_id === s.id && a.status === "open").length}
                  />
                ))}
              </div>
            </section>
          );
        })}
      </div>
    </>
  );
}

function SessionCard({ s, openAttention }: { s: Session; openAttention: number }) {
  const { selectedSession, selectSession, setView } = useUi();
  const command = useCommand();
  const [draft, setDraft] = useState("");
  const busy = command.isPending;
  const isActive = ACTIVE_STATES.includes(s.state);
  const selected = selectedSession === s.id;

  const canSend = isSupported(s.capabilities.mid_turn_steer) && s.state !== "starting";
  const canCancel = isSupported(s.capabilities.cancel) && isActive;
  const canResume = isSupported(s.capabilities.resume) && !isActive;
  const tokens = s.usage ? fmtTokens(s.usage) : null;
  const send = () => {
    const t = draft.trim();
    if (!t) return;
    command.mutate(sendSessionMessage(s.id, t));
    setDraft("");
  };

  return (
    <div className={`card session ${selected ? "selected" : ""}`}>
      <div className="head">
        <ProviderAvatar s={s} />
        <div className="who">
          <span className="title">{s.title}</span>
          <span className="meta">{PROVIDER_LABEL[s.provider]} · {s.ownership}</span>
        </div>
        <button
          className="icon-btn"
          title={selected ? "Close inspector" : "Inspect"}
          onClick={() => selectSession(selected ? null : s.id)}
        >
          <ArrowUpRight size={16} strokeWidth={1.9} />
        </button>
      </div>

      <div className="status">
        <StateChip state={s.state} />
        {s.status_detail && (
          <span
            className={`chip ${s.status_detail.kind === "inference" ? "warn" : ""}`}
            title={s.status_detail.kind === "inference" ? "Inferred by Orc — not reported by the provider" : "Reported by the provider"}
          >
            {s.status_detail.kind === "inference" && <Wand2 size={11} />}
            {s.status_detail.message}
          </span>
        )}
        {openAttention > 0 && (
          <button className="chip warn" onClick={() => setView("inbox")}>{openAttention} needs you</button>
        )}
      </div>

      {s.last_meaningful_update && (
        <div className="latest selectable">
          <span className="when">Latest · {ago(s.updated_at)}</span>
          <span className="text">{s.last_meaningful_update}</span>
        </div>
      )}

      <div className="foot">
        {canSend ? (
          <>
            <input
              className="field"
              placeholder={isActive ? "Steer this turn…" : "Send a follow-up…"}
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") send(); }}
            />
            {draft.trim() && (
              <button className="icon-btn solid" disabled={busy} onClick={send} aria-label="Send">
                <ArrowUp size={15} strokeWidth={2.2} />
              </button>
            )}
          </>
        ) : (
          <span className="stats">
            {[s.activity_started_at && isActive ? fmtElapsed(s.activity_started_at) : null, tokens].filter(Boolean).join(" · ") || ago(s.updated_at)}
          </span>
        )}
        {canCancel && (
          <button className="pill sm danger" disabled={busy} onClick={() => command.mutate(cancelSession(s.id))}>
            <Square size={11} fill="currentColor" />Stop
          </button>
        )}
        {canResume && (
          <button className="pill sm" disabled={busy} onClick={() => command.mutate(resumeSession(s.id))}>
            <Play size={11} fill="currentColor" />Resume
          </button>
        )}
      </div>
    </div>
  );
}
