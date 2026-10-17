import { useUi } from "../store";
import { transport } from "../transport";
import type { Snapshot } from "../types";

export function Sidebar({ snap }: { snap: Snapshot }) {
  const { view, setView, selectSession, selectedSession } = useUi();
  const openAttention = snap.attention.filter((a) => a.status === "open").length;
  const active = snap.sessions.filter((s) =>
    ["starting", "running", "waiting_for_input", "waiting_for_approval"].includes(s.state),
  );

  return (
    <aside className="sidebar">
      <div className="brand"><span className="dot" />Orc</div>
      <div className={`conn ${transport.label === "demo" ? "demo" : ""}`}>
        {transport.label === "demo" ? "demo mode — daemon not connected" : "daemon connected"}
      </div>

      <nav className="nav">
        <button className={`nav-item ${view === "chat" ? "active" : ""}`} onClick={() => setView("chat")}>
          Chat
        </button>
        <button className={`nav-item ${view === "live" ? "active" : ""}`} onClick={() => setView("live")}>
          Live
          {active.length > 0 && <span className="count">{active.length}</span>}
        </button>
        <button className={`nav-item ${view === "inbox" ? "active" : ""}`} onClick={() => setView("inbox")}>
          Attention
          {openAttention > 0 && <span className="count">{openAttention}</span>}
        </button>
      </nav>

      <div>
        <div className="side-label">Sessions</div>
        <div className="side-list">
          {snap.sessions.slice(0, 12).map((s) => (
            <div
              key={s.id}
              className={`side-row ${selectedSession === s.id ? "active" : ""}`}
              onClick={() => { selectSession(s.id); setView("live"); }}
            >
              <StateDot state={s.state} />
              <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{s.title}</span>
            </div>
          ))}
          {snap.sessions.length === 0 && <div className="side-row" style={{ cursor: "default" }}>No sessions</div>}
        </div>
      </div>

      <div>
        <div className="side-label">Tools</div>
        <div className="side-list">
          {snap.tools.map((t) => (
            <div key={t.id} className="side-row" style={{ cursor: "default" }}>
              <span className="dot" style={{
                width: 7, height: 7, borderRadius: "50%",
                background: t.readiness.type === "ready" ? "var(--good)" : "var(--warn)",
              }} />
              <span>{t.label}</span>
              <span style={{ marginLeft: "auto", color: "var(--ink-3)" }}>{t.version ?? ""}</span>
            </div>
          ))}
        </div>
      </div>
    </aside>
  );
}

export function StateDot({ state }: { state: string }) {
  const color =
    state === "running" ? "var(--good)"
    : state === "waiting_for_approval" || state === "waiting_for_input" ? "var(--warn)"
    : state === "failed" || state === "unknown" ? "var(--bad)"
    : "var(--ink-3)";
  return <span style={{ width: 7, height: 7, borderRadius: "50%", background: color, flexShrink: 0 }} />;
}
