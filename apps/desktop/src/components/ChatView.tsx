import { Activity, ArrowUp, CircleCheck, CircleX, History, Inbox, Orbit, RotateCcw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useChatHistory, useSendChat } from "../api";
import type { ChatMessage, OperationCard } from "../types";

const SUGGESTIONS = [
  { icon: Activity, text: "What is running right now?" },
  { icon: Inbox, text: "What needs a decision from me?" },
  { icon: History, text: "What did the agents accomplish while I was away?" },
  { icon: RotateCcw, text: "Resume yesterday's session" },
];

export function ChatView() {
  const history = useChatHistory();
  const send = useSendChat();
  const [text, setText] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const messages = history.data ?? [];
  const empty = messages.length === 0 && !send.isPending;

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  }, [messages.length, send.isPending]);

  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "0px";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, [text]);

  const submit = (value = text) => {
    const t = value.trim();
    if (!t || send.isPending) return;
    setText("");
    send.mutate(t);
  };

  return (
    <div className="chat">
      <div className="topbar drag" data-tauri-drag-region>
        <h1>Chat</h1>
        <span className="sub">Orchestrator</span>
      </div>
      <div className="scroll" ref={scrollRef}>
        {empty ? (
          <div className="greeting">
            <h2>What should the agents work on?</h2>
            <p>Start sessions, check on running work, or route a decision.</p>
            <div className="suggestions">
              {SUGGESTIONS.map(({ icon: Icon, text: s }) => (
                <button key={s} className="suggestion" onClick={() => submit(s)}>
                  <span className="avatar"><Icon size={15} strokeWidth={1.9} /></span>
                  {s}
                </button>
              ))}
            </div>
          </div>
        ) : (
          <div className="thread">
            {messages.map((m) => <Message key={m.id} m={m} />)}
            {send.isPending && (
              <div className="msg-orc">
                <OrcAvatar />
                <div className="body"><span className="typing"><i /><i /><i /></span></div>
              </div>
            )}
          </div>
        )}
      </div>
      <div className="composer-wrap">
        <div className="composer">
          <textarea
            ref={inputRef}
            rows={1}
            placeholder="Ask Orc to start, check on, or stop work…"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                submit();
              }
            }}
          />
          <div className="bar">
            <span className="chip">Typed tools only</span>
            <span className="spacer" />
            <button className="icon-btn solid" onClick={() => submit()} disabled={send.isPending || !text.trim()} aria-label="Send">
              <ArrowUp size={16} strokeWidth={2.2} />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

function OrcAvatar() {
  return <span className="avatar sm ink"><Orbit size={14} strokeWidth={2} /></span>;
}

function Message({ m }: { m: ChatMessage }) {
  const text = m.content?.text ?? "";
  const ops: OperationCard[] = m.content?.operations ?? [];
  if (m.role === "user") return <div className="msg-user selectable">{text}</div>;
  return (
    <div className="msg-orc">
      <OrcAvatar />
      <div className="body">
        {text && <div className={`text selectable ${m.role === "system" ? "dim" : ""}`}>{text}</div>}
        {ops.map((op) => <Operation key={op.operation_id} op={op} />)}
      </div>
    </div>
  );
}

function Operation({ op }: { op: OperationCard }) {
  const result = op.result?.type;
  const ok = result === "applied";
  const failed = result === "denied" || result === "rejected";
  return (
    <div className="op">
      <span className={`avatar sm ${failed ? "bad" : ""}`}>
        {failed ? <CircleX size={14} /> : <CircleCheck size={14} />}
      </span>
      <div className="what">
        <span className="kind">{op.kind}</span>
        <span className="display">{op.display}</span>
      </div>
      {result && (
        <span className={`chip ${failed ? "bad" : result === "outcome_unknown" ? "warn" : ""}`}>
          {ok ? "Applied" : result.replaceAll("_", " ")}
        </span>
      )}
    </div>
  );
}
