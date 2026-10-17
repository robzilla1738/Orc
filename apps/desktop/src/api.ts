// Runtime data layer: snapshot + resyncable event stream on top of
// TanStack Query. Commands go through the same IPC `command.exec`
// channel as the CLI — one policy path.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";
import { transport } from "./transport";
import type {
  ChatMessage, ChatReply, Command, CommandOutcome, ServerEvent, Snapshot,
} from "./types";
import { UNKNOWN_CAPABILITIES } from "./types";

export function useSnapshot() {
  const qc = useQueryClient();
  const query = useQuery({
    queryKey: ["snapshot"],
    queryFn: () => transport.request("snapshot.get") as Promise<Snapshot>,
    refetchOnWindowFocus: true,
  });

  useEffect(() => {
    let cancelled = false;
    let unsubscribe: (() => void) | null = null;
    const onEvent = (e: ServerEvent) => {
      qc.setQueryData<Snapshot>(["snapshot"], (snap) => (snap ? applyEvent(snap, e) : snap));
    };
    const onReset = () => qc.invalidateQueries({ queryKey: ["snapshot"] });
    transport.subscribe(onEvent, onReset).then((u) => {
      if (cancelled) u();
      else unsubscribe = u;
    });
    return () => {
      cancelled = true;
      unsubscribe?.();
    };
  }, [qc]);

  return query;
}

// Fold durable events into the snapshot's current-state projection.
// Projection tables are authoritative in the daemon; the client keeps a
// lightweight mirror so the UI reacts between snapshot reloads.
function applyEvent(snap: Snapshot, e: ServerEvent): Snapshot {
  const p = e.payload as { type: string; [k: string]: any };
  const next = { ...snap, cursor: e.seq };
  switch (p.type) {
    case "session_state_changed": {
      const sid = p.session_id as string;
      next.sessions = snap.sessions.map((s) =>
        s.id === sid
          ? {
              ...s,
              state: p.to ?? s.state,
              status_detail: p.status_detail ?? null,
              updated_at: e.at,
              activity_started_at: p.activity_started_at ?? s.activity_started_at,
            }
          : s,
      );
      return next;
    }
    case "session_created": {
      const sid = p.session_id as string;
      if (snap.sessions.some((s) => s.id === sid)) return next;
      next.sessions = [
        {
          id: sid,
          workspace_id: p.workspace_id ?? "",
          task_id: p.task_id ?? null,
          tool_instance_id: p.tool_instance_id ?? "",
          provider: p.provider ?? "other",
          ownership: p.ownership ?? "managed",
          native_id: null,
          title: p.title ?? "Untitled session",
          state: "starting",
          status_detail: null,
          capabilities: UNKNOWN_CAPABILITIES,
          pid: null,
          last_meaningful_update: null,
          usage: null,
          last_event_seq: e.seq,
          created_at: e.at,
          updated_at: e.at,
          activity_started_at: null,
        },
        ...snap.sessions,
      ];
      return next;
    }
    case "session_updated": {
      const sid = p.session_id as string;
      next.sessions = snap.sessions.map((s) =>
        s.id === sid
          ? {
              ...s,
              last_meaningful_update: p.last_meaningful_update ?? s.last_meaningful_update,
              usage: p.usage ?? s.usage,
              updated_at: e.at,
            }
          : s,
      );
      return next;
    }
    case "attention_raised": {
      const item = p.item;
      next.attention = [item, ...snap.attention.filter((a) => a.id !== item.id)];
      return next;
    }
    case "attention_resolved": {
      const id = p.attention_id as string;
      next.attention = snap.attention.map((a) =>
        a.id === id ? { ...a, status: p.status ?? "answered" } : a,
      );
      return next;
    }
    case "tool_discovered": {
      const tool = p.tool;
      next.tools = [tool, ...snap.tools.filter((t) => t.id !== tool.id)];
      return next;
    }
    case "task_created": {
      next.tasks = [p.task, ...snap.tasks.filter((t) => t.id !== p.task.id)];
      return next;
    }
    case "task_updated": {
      next.tasks = snap.tasks.map((t) => (t.id === p.task.id ? p.task : t));
      return next;
    }
    default:
      return next;
  }
}

// ---- commands ---------------------------------------------------------

let opCounter = 0;
function newId(prefix: string) {
  return `${prefix}_${Date.now().toString(36)}${(opCounter++).toString(36)}`;
}

export function useCommand() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (kind: Command["kind"]) => {
      const cmd: Command = {
        id: newId("op"),
        idempotency_key: newId("idem"),
        actor: "user",
        kind,
      };
      const res = (await transport.request("command.exec", cmd)) as CommandOutcome;
      if (res.type === "denied") throw new Error(res.reason ?? "denied");
      if (res.type === "rejected") throw new Error(res.reason ?? "rejected");
      return res;
    },
    onSettled: () => qc.invalidateQueries({ queryKey: ["snapshot"] }),
  });
}

export function sendSessionMessage(sessionId: string, text: string) {
  return { type: "session_send", session_id: sessionId, text } as Command["kind"];
}
export function cancelSession(sessionId: string) {
  return { type: "session_cancel", session_id: sessionId } as Command["kind"];
}
export function resumeSession(sessionId: string) {
  return { type: "session_resume", session_id: sessionId } as Command["kind"];
}
export type Answer =
  | { type: "text"; text: string }
  | { type: "choice"; index: number }
  | { type: "approve"; scope: "once" | "for_run" | "workspace_rule" }
  | { type: "deny"; reason?: string };

export function answerAttention(attentionId: string, answer: Answer) {
  return { type: "attention_answer", attention_id: attentionId, answer } as Command["kind"];
}
export function dismissAttention(attentionId: string) {
  return { type: "attention_dismiss", attention_id: attentionId } as Command["kind"];
}
export function startSession(toolInstanceId: string, workspaceId: string, title: string, prompt: string) {
  return {
    type: "session_start",
    tool_instance_id: toolInstanceId,
    workspace_id: workspaceId,
    task_id: null,
    title,
    prompt,
    launch: {},
  } as Command["kind"];
}

// ---- chat -------------------------------------------------------------

export function useChatHistory() {
  return useQuery({
    queryKey: ["chat"],
    queryFn: () => transport.request("chat.history") as Promise<ChatMessage[]>,
  });
}

export function useSendChat() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (text: string) =>
      transport.request("chat.send", { text }) as Promise<ChatReply>,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["chat"] });
      qc.invalidateQueries({ queryKey: ["snapshot"] });
    },
  });
}

export function useTranscript(sessionId: string | null) {
  return useQuery({
    queryKey: ["transcript", sessionId],
    enabled: !!sessionId,
    queryFn: () =>
      transport.request("transcript.tail", {
        session_id: sessionId,
        stream: "stdout",
        max_bytes: 64 * 1024,
      }) as Promise<{ text: string; cursor: number }>,
    refetchInterval: sessionId ? 3000 : false,
  });
}
