import { Check, CircleHelp, FileSearch, Inbox, Lock, ShieldAlert, TriangleAlert, Wand2, X } from "lucide-react";
import { useState } from "react";
import { answerAttention, dismissAttention, useCommand, type Answer } from "../api";
import type { AttentionItem, Snapshot } from "../types";
import { ago } from "./ui";

const KIND_LABEL: Record<string, string> = {
  question: "Question",
  approval: "Approval",
  auth_failed: "Authentication failed",
  review_request: "Review requested",
  uncertain_outcome: "Outcome unknown",
  inferred_prompt: "Possible prompt · inferred",
};

function KindAvatar({ kind }: { kind: string }) {
  const p = { size: 17, strokeWidth: 1.9 };
  switch (kind) {
    case "approval": return <span className="avatar ink"><ShieldAlert {...p} /></span>;
    case "question": return <span className="avatar"><CircleHelp {...p} /></span>;
    case "review_request": return <span className="avatar"><FileSearch {...p} /></span>;
    case "inferred_prompt": return <span className="avatar warn"><Wand2 {...p} /></span>;
    default: return <span className="avatar bad"><TriangleAlert {...p} /></span>;
  }
}

export function InboxView({ snap }: { snap: Snapshot }) {
  const open = snap.attention.filter((a) => a.status === "open");
  const resolved = snap.attention.filter((a) => a.status !== "open");

  return (
    <>
      <div className="topbar drag" data-tauri-drag-region>
        <h1>Attention</h1>
        <span className="sub">{open.length === 0 ? "Nothing waiting" : `${open.length} waiting on you`}</span>
      </div>
      <div className="scroll inbox">
        <div className="inbox-list">
          {open.length === 0 && (
            <div className="empty">
              <span className="avatar"><Inbox size={18} /></span>
              <div className="title">All clear</div>
              <div className="body">Approvals, questions, and uncertain outcomes land here — and in chat.</div>
            </div>
          )}
          {open.map((a) => <AttentionCard key={a.id} a={a} snap={snap} />)}
          {resolved.length > 0 && (
            <>
              <div className="section-label">Resolved</div>
              {resolved.slice(0, 12).map((a) => (
                <div key={a.id} className="resolved-row">
                  {a.status === "answered" ? <Check size={14} /> : <X size={14} />}
                  <span className="t">{a.title}</span>
                  <span>{a.status.replaceAll("_", " ")}</span>
                </div>
              ))}
            </>
          )}
        </div>
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
  const sendText = () => {
    if (!text.trim()) return;
    answer({ type: "text", text: text.trim() });
    setText("");
  };

  return (
    <div className="card att">
      <div className="head">
        <KindAvatar kind={a.kind} />
        <div className="who">
          <span className="title">{a.title}</span>
          <span className="meta">
            {[KIND_LABEL[a.kind] ?? a.kind, session?.title, ago(a.created_at)].filter(Boolean).join(" · ")}
          </span>
        </div>
      </div>

      {a.body && a.body.trim() !== a.pending_action?.display.trim() && <div className="body selectable">{a.body}</div>}

      {a.pending_action && (
        <div className="action">
          <span className="label"><Lock size={11} />Bound to this exact action</span>
          <code className="selectable">{a.pending_action.display}</code>
        </div>
      )}

      {a.kind === "inferred_prompt" && (
        <span className="chip warn" style={{ alignSelf: "flex-start" }}>
          <Wand2 size={11} />Inferred from terminal output — verify before answering
        </span>
      )}

      <div className="actions">
        {a.kind === "approval" || a.kind === "review_request" ? (
          <>
            <button className="pill primary" disabled={busy} onClick={() => answer({ type: "approve", scope: "once" })}>
              Approve once
            </button>
            {a.kind === "approval" && (
              <button className="pill" disabled={busy} onClick={() => answer({ type: "approve", scope: "workspace_rule" })}>
                Always allow here
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
              className="field"
              placeholder="Type an answer…"
              value={text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") sendText(); }}
            />
            <button className="pill primary" disabled={busy || !text.trim()} onClick={sendText}>Answer</button>
            <button className="pill quiet" disabled={busy} onClick={() => command.mutate(dismissAttention(a.id))}>Dismiss</button>
          </>
        )}
      </div>
    </div>
  );
}
