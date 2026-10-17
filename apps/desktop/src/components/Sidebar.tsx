import type { ReactNode } from "react";
import { Activity, Inbox, MessageCircle, Orbit } from "lucide-react";
import { useUi, type View } from "../store";
import { transport } from "../transport";
import type { Snapshot } from "../types";
import { ACTIVE_STATES, StateDot } from "./ui";

export function Sidebar({ snap }: { snap: Snapshot }) {
  const { view, setView, selectSession, selectedSession } = useUi();
  const openAttention = snap.attention.filter((a) => a.status === "open").length;
  const active = snap.sessions.filter((s) => ACTIVE_STATES.includes(s.state)).length;
  const demo = transport.label === "demo";
  const readyTools = snap.tools.filter((t) => t.readiness === "ready").length;

  const nav: { id: View; label: string; icon: typeof Activity; trail?: ReactNode }[] = [
    { id: "chat", label: "Chat", icon: MessageCircle },
    { id: "live", label: "Live", icon: Activity, trail: active > 0 ? <span className="trail">{active}</span> : null },
    { id: "inbox", label: "Attention", icon: Inbox, trail: openAttention > 0 ? <span className="badge">{openAttention}</span> : null },
  ];

  return (
    <aside className="sidebar">
      <div className="titlebar drag" data-tauri-drag-region />
      <div className="brand">
        <span className="mark"><Orbit size={15} strokeWidth={2} /></span>
        <span className="name">Orc</span>
        <span
          className={`conn ${demo ? "demo" : ""}`}
          title={demo ? "Demo data — daemon not connected" : "Connected to orcd"}
        />
      </div>
      {demo && <div className="demo-note">Demo data — daemon not connected</div>}

      <div className="side-scroll">
        <nav className="nav">
          {nav.map(({ id, label, icon: Icon, trail }) => (
            <button key={id} className={`row ${view === id ? "active" : ""}`} onClick={() => setView(id)}>
              <Icon size={16} strokeWidth={1.9} />
              <span className="label">{label}</span>
              {trail}
            </button>
          ))}
        </nav>

        <div>
          <div className="side-label">Sessions</div>
          <div className="nav">
            {snap.sessions.slice(0, 14).map((s) => (
              <button
                key={s.id}
                className={`row ${view === "live" && selectedSession === s.id ? "active" : ""}`}
                onClick={() => { setView("live"); selectSession(s.id); }}
              >
                <StateDot state={s.state} />
                <span className="label">{s.title}</span>
              </button>
            ))}
            {snap.sessions.length === 0 && <div className="side-empty">No sessions yet</div>}
          </div>
        </div>

        <div>
          <div className="side-label">Tools</div>
          <div className="nav">
            {snap.tools.map((t) => (
              <div key={t.id} className="row" title={`${t.executable} — ${t.readiness}`}>
                <span className={`dot ${t.readiness === "ready" ? "running" : "waiting"}`} style={{ animation: "none" }} />
                <span className="label">{t.label}</span>
                <span className="trail">{t.readiness}</span>
              </div>
            ))}
            {snap.tools.length === 0 && <div className="side-empty">No tools discovered</div>}
          </div>
        </div>
      </div>

      <div className="side-foot">
        {readyTools}/{snap.tools.length} tools ready · {snap.workspaces.length} workspace{snap.workspaces.length === 1 ? "" : "s"}
      </div>
    </aside>
  );
}
