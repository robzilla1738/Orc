import { cancelSession, resumeSession, sendSessionMessage, useCommand } from "../api";
import { useUi } from "../store";
import type { Session, Snapshot } from "../types";
import { StateDot } from "./Sidebar";
import { useState } from "react";

const ACTIVE = ["starting", "running", "waiting_for_input", "waiting_for_approval"];

export function LiveView({ snap }: { snap: Snapshot }) {
  const byWorkspace = new Map<string, Session[]>();
  for (const s of snap.sessions) {
    const list = byWorkspace.get(s.workspace_id) ?? [];
    list.push(s);
    byWorkspace.set(s.workspace_id, list);
  }

  return (
    <>
      <div className="topbar">
        <h1>Live</h1>
        <span className="chip">{snap.sessions.filter((s) => ACTIVE.includes(s.state)).length} active</span>
      </div>
      <div className="live scroll-fade">
        {snap.sessions.length === 0 && (
          <div className="empty">
            <div className="big">Nothing running</div>
            <div>Sessions you start — from chat or the tool list — show up here.</div>
          </div>
        )}
        {[...byWorkspace.entries()].map(([wid, sessions]) => {
          const ws = snap.workspaces.find((w) => w.id === wid);
          return (
            <section key={wid} className="ws-group">
              <div className="ws-head">
                <span className="name">{ws?.name ?? "workspace"}</span>
                <span className="root">{ws?.root}</span>
              </div>
              <div className="sessions">
                {sessions.map((s) => (
                  <SessionCard key={s.id} s={s} attention={snap.attention.filter((a) => a.session_id === s.id && a.status === "open")} />
                ))}
              </div>
            </section>
          );
        })}
      </div>
    </>
  );
}

function SessionCard({ s, attention }: { s: Session; attention: { id: string }[] }) {
  const { selectedSession, selectSession } = useUi();
  const command = useCommand();
  const [draft, setDraft] = useState("");
  const busy = command.isPending;

  const stateChip =
    s.state === "running" ? "running"
    : s.state.startsWith("waiting") ? "attention"
    : s.state === "failed" || s.state === "unknown" ? "error"
    : "inactive";

  const elapsed = s.activity_started_at ? fmtElapsed(s.activity_started_at) : null;
  const canSend = s.capabilities.send.type === "supported" && s.state !== "starting";
  const canCancel = s.capabilities.cancel.type === "supported" && ACTIVE.includes(s.state);
  const canResume = s.capabilities.resume.type === "supported" && !ACTIVE.includes(s.state);

  return (
    <div
      className={`card session-card ${selectedSession === s.id ? "selected" : ""}`}
      onClick={() => selectSession(s.id)}
    >
      <div className="top">
        <StateDot state={s.state} />
        <span className="title">{s.title}</span>
        <span className={`chip ${stateChip}`}><span className="pulse" />{s.state.replaceAll("_", " ")}</span>
      </div>

      <div className="meta">
        <span>{s.provider}</span>
        <span className="chip">{s.ownership}</span>
        {elapsed && <span>{elapsed}</span>}
        {attention.length > 0 && <span className="chip attention"><span className="pulse" />{attention.length} needs you</span>}
        {s.usage?.input_tokens != null && (
          <span>{s.usage.source === "provider" ? "" : "≈"}{fmtTokens(s.usage)}</span>
        )}
      </div>

      {s.status_detail && (
        <span className={s.status_detail.kind === "fact" ? "badge-fact" : "badge-inference"}>
          {s.status_detail.kind === "inference" ? "⬦ " : ""}{s.status_detail.message}
        </span>
      )}

      {s.last_meaningful_update && <div className="update">{s.last_meaningful_update}</div>}

      <div className="controls" onClick={(e) => e.stopPropagation()}>
        {canSend && (
          <>
            <input
              type="text"
              className="pill ghost"
              style={{ flex: 1, minWidth: 0, background: "rgba(0,0,0,0.25)", outline: "none" }}
              placeholder="message…"
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && draft.trim()) {
                  command.mutate(sendSessionMessage(s.id, draft.trim()));
                  setDraft("");
                }
              }}
            />
            <button
              className="pill small"
              disabled={busy || !draft.trim()}
              onClick={() => { command.mutate(sendSessionMessage(s.id, draft.trim())); setDraft(""); }}
            >
              Send
            </button>
          </>
        )}
        {canCancel && (
          <button className="pill small danger" disabled={busy} onClick={() => command.mutate(cancelSession(s.id))}>
            Stop
          </button>
        )}
        {canResume && (
          <button className="pill small" disabled={busy} onClick={() => command.mutate(resumeSession(s.id))}>
            Resume
          </button>
        )}
      </div>
    </div>
  );
}

export function fmtElapsed(startedIso: string): string {
  const secs = Math.max(0, (Date.now() - Date.parse(startedIso)) / 1000);
  if (secs < 60) return `${Math.floor(secs)}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

function fmtTokens(u: { input_tokens: number | null; output_tokens: number | null }): string {
  const i = u.input_tokens ?? 0;
  const o = u.output_tokens ?? 0;
  if (i + o >= 1_000_000) return `${((i + o) / 1_000_000).toFixed(1)}M tok`;
  if (i + o >= 1000) return `${((i + o) / 1000).toFixed(1)}k tok`;
  return `${i + o} tok`;
}
