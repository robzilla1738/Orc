import { useEffect, useRef, useState } from "react";
import { useChatHistory, useSendChat } from "../api";
import type { ChatMessage, OperationCard } from "../types";

export function ChatView() {
  const history = useChatHistory();
  const send = useSendChat();
  const [text, setText] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);

  const messages = history.data ?? [];

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight });
  }, [messages.length, send.isPending]);

  const submit = () => {
    const t = text.trim();
    if (!t || send.isPending) return;
    setText("");
    send.mutate(t);
  };

  return (
    <>
      <div className="topbar">
        <h1>Chat</h1>
        <span className="chip"><span className="pulse" style={{ background: "var(--accent)" }} />orchestrator</span>
      </div>
      <div className="chat-scroll scroll-fade" ref={scrollRef}>
        {messages.map((m) => <Message key={m.id} m={m} />)}
        {send.isPending && (
          <div className="msg assistant">
            <div className="role">orc</div>
            <div className="bubble dim">thinking…</div>
          </div>
        )}
        {messages.length === 0 && !send.isPending && (
          <div className="empty">
            <div className="big">What are we working on?</div>
            <div>Start a session, check on running work, or route a decision.</div>
          </div>
        )}
      </div>
      <div className="composer">
        <textarea
          rows={1}
          placeholder="Message orc — “what is running?”, “resume the auth session”, …"
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
        />
        <button className="pill primary" onClick={submit} disabled={send.isPending || !text.trim()}>
          Send
        </button>
      </div>
    </>
  );
}

function Message({ m }: { m: ChatMessage }) {
  const text = m.content?.text ?? "";
  const ops: OperationCard[] = m.content?.operations ?? [];
  return (
    <div className={`msg ${m.role}`}>
      <div className="role">{m.role === "user" ? "you" : "orc"}</div>
      {text && <div className="bubble">{text}</div>}
      {ops.map((op) => (
        <div key={op.operation_id} className="op-card">
          <span className="kind">{op.kind}</span>
          <span>{op.display}</span>
          {op.result && <span style={{ marginLeft: "auto", opacity: 0.7 }}>{op.result.type}</span>}
        </div>
      ))}
    </div>
  );
}
