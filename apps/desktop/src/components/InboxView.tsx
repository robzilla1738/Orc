import { answerAttention, dismissAttention, useCommand, type Answer } from "../api";
import type { AttentionItem, Snapshot } from "../types";
import { useState } from "react";

const KIND_LABEL: Record<string, string> = {
  question: "Question",
  approval: "Approval",
  auth_failed: "Authentication failed",
  review_request: "Review requested",
  uncertain_outcome: "Uncertain outcome",
  inferred_prompt: "Possible prompt (inferred)",
};

export function InboxView({ snap }: { snap: Snapshot }) {
  const open = snap.attention.filter((a) => a.status === "open");
  const resolved = snap.attention.filter((a) => a.status !== "open");

  return (
    <>
      <div className="topbar">
        <h1>Attention</h1>
        {open.length > 0 && <span className="chip attention"><span className="pulse" />{open.length} open</span>}
      </div>
      <div className="inbox scroll-fade">
        {open.length === 0 && (
          <div className="empty">
            <div className="big">All clear</div>
            <div>Approvals, questions, and uncertain outcomes land here — and in chat.</div>
          </div>
        )}
        {open.map((a) => <AttentionCard key={a.id} a={a} snap={snap} />)}
        {resolved.length > 0 && (
          <>
            <div className="side-label" style={{ padding: "8px 4px 0" }}>Resolved</div>
            {resolved.slice(0, 10).map((a) => <AttentionCard key={a.id} a={a} snap={snap} />)}
          </>
        )}
      </div>
    </>
  );
}

function AttentionCard({ a, snap }: { a: AttentionItem; snap: Snapshot }) {
  const command = useCommand();
  const [text, setText] = useState("");
  const busy = command.isPending;
  const session = a.session_id ? snap.sessions.find((s) => s.id === a.session_id) : null;
  const answer = (ans: Answer) => command.mutate(answerAttention(a.id, ans));

  return (
    <div className={`card att-card ${a.status !== "open" ? "dim" : ""}`}>
      <div className="head">
        <span className={`kind ${a.kind}`}>{KIND_LABEL[a.kind] ?? a.kind}</span>
        <span className="title">{a.title}</span>
        {session && <span className="chip">{session.title}</span>}
      </div>

      {a.body && <div className="body">{a.body}</div>}

      {a.pending_action && (
        <code>{a.pending_action.display}</code>
      )}

      {a.kind === "inferred_prompt" && (
        <span className="badge-inference">
          inferred from terminal output — not a structured request; verify before answering
        </span>
      )}

      {a.status === "open" ? (
        <div className="actions">
          {a.kind === "approval" || a.kind === "review_request" ? (
            <>
              <button className="pill primary" disabled={busy} onClick={() => answer({ type: "approve", scope: "once" })}>
                Approve once
              </button>
              {a.kind === "approval" && (
                <button className="pill" disabled={busy} onClick={() => answer({ type: "approve", scope: "workspace_rule" })}>
                  Always in this workspace
                </button>
              )}
              <button className="pill danger" disabled={busy} onClick={() => answer({ type: "deny" })}>
                Deny
              </button>
            </>
          ) : a.kind === "auth_failed" || a.kind === "uncertain_outcome" ? (
            <button className="pill" disabled={busy} onClick={() => command.mutate(dismissAttention(a.id))}>
              Acknowledge
            </button>
          ) : a.choices.length > 0 ? (
            a.choices.map((c, i) => (
              <button key={i} className="pill" disabled={busy} onClick={() => answer({ type: "choice", index: i })}>
                {c}
              </button>
            ))
          ) : (
            <>
              <input
                type="text"
                placeholder="answer…"
                value={text}
                onChange={(e) => setText(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && text.trim()) { answer({ type: "text", text: text.trim() }); setText(""); }
                }}
              />
              <button className="pill primary" disabled={busy || !text.trim()} onClick={() => { answer({ type: "text", text: text.trim() }); setText(""); }}>
                Answer
              </button>
              <button className="pill ghost" disabled={busy} onClick={() => command.mutate(dismissAttention(a.id))}>
                Dismiss
              </button>
            </>
          )}
        </div>
      ) : (
        <span className="resolved">{a.status}</span>
      )}
    </div>
  );
}
