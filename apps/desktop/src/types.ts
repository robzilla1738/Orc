// Wire types — mirror crates/domain snake_case serialization.

export type Id = string;

export type SessionState =
  | "starting" | "idle" | "running" | "waiting_for_input"
  | "waiting_for_approval" | "completed" | "failed" | "cancelled" | "unknown";

export type Ownership = "managed" | "imported" | "observed";
export type ProviderKind = "codex" | "acp" | "claude" | "opencode" | "pty" | "other";

export interface StatusDetail {
  kind: "fact" | "inference";
  message: string;
}

export interface Usage {
  input_tokens: number | null;
  output_tokens: number | null;
  cost_usd: number | null;
  source: "provider" | "estimate" | "unavailable";
}

export interface Capabilities {
  list_sessions: Support;
  import_session: Support;
  resume: Support;
  fork: Support;
  streaming: Support;
  structured_approvals: Support;
  structured_questions: Support;
  cancel: Support;
  mid_turn_steer: Support;
  reconnect: Support;
  usage_reporting: Support;
  artifact_reporting: Support;
}

const UNKNOWN: Support = { status: "unknown" };

export const UNKNOWN_CAPABILITIES: Capabilities = {
  list_sessions: UNKNOWN,
  import_session: UNKNOWN,
  resume: UNKNOWN,
  fork: UNKNOWN,
  streaming: UNKNOWN,
  structured_approvals: UNKNOWN,
  structured_questions: UNKNOWN,
  cancel: UNKNOWN,
  mid_turn_steer: UNKNOWN,
  reconnect: UNKNOWN,
  usage_reporting: UNKNOWN,
  artifact_reporting: UNKNOWN,
};

export type Support =
  | { status: "supported"; constraint?: string | null }
  | { status: "unsupported"; reason: string }
  | { status: "unknown" };

export function isSupported(s: Support | undefined | null): boolean {
  return s?.status === "supported";
}

export interface Session {
  id: Id;
  workspace_id: Id;
  task_id: Id | null;
  tool_instance_id: Id;
  provider: ProviderKind;
  ownership: Ownership;
  native_id: string | null;
  title: string;
  state: SessionState;
  status_detail: StatusDetail | null;
  capabilities: Capabilities;
  pid: number | null;
  last_meaningful_update: string | null;
  usage: Usage | null;
  process_started_at: string | null;
  last_event_seq: number;
  created_at: string;
  updated_at: string;
  activity_started_at: string | null;
}

export type AttentionKind =
  | "question" | "approval" | "auth_failed" | "review_request"
  | "uncertain_outcome" | "inferred_prompt";

export type AttentionStatus = "open" | "answered" | "dismissed" | "expired";

export interface PendingAction {
  kind: string;
  display: string;
  args: unknown;
  args_hash: string;
  provider_ref: string | null;
}

export interface AttentionItem {
  id: Id;
  session_id: Id | null;
  run_id: Id | null;
  workspace_id: Id;
  kind: AttentionKind;
  status: AttentionStatus;
  title: string;
  body: string;
  pending_action: PendingAction | null;
  choices: string[];
  provider_request_ref: string | null;
  expires_at: string | null;
  expires_with_session: boolean;
  created_at: string;
}

export interface Workspace {
  id: Id;
  name: string;
  root: string;
  allow_rules: { action_kind: string; pattern: string; created_at: string }[];
  network_allowed: boolean;
  remote_content_allowed: boolean;
}

export type TaskState = "planned" | "running" | "blocked" | "done" | "failed";
export type StepState = "pending" | "ready" | "running" | "done" | "failed" | "skipped";

export interface WorkflowStep {
  id: Id;
  title: string;
  owner: string;
  state: StepState;
  depends_on: Id[];
  session_id: Id | null;
  attempts: number;
  budget_secs: number | null;
}

export interface Task {
  id: Id;
  workspace_id: Id;
  title: string;
  description: string | null;
  state: TaskState;
  steps: WorkflowStep[];
  created_at: string;
  updated_at: string;
}

export type ToolReadiness =
  | { type: "ready" }
  | { type: "installed_unauthenticated" }
  | { type: "installed_unprobed" }
  | { type: "incompatible"; reason: string }
  | { type: "missing" };

export interface ToolInstance {
  id: Id;
  provider: ProviderKind;
  label: string;
  executable: string;
  version: string | null;
  readiness: ToolReadiness;
  settings: Record<string, string>;
}

export interface PendingIntent {
  operation_id: Id;
  actor: string;
  kind: { type: string };
}

export interface Snapshot {
  cursor: number;
  sessions: Session[];
  runs: unknown[];
  attention: AttentionItem[];
  tasks: Task[];
  tools: ToolInstance[];
  workspaces: Workspace[];
  pending_commands: PendingIntent[];
}

export interface ServerEvent {
  seq: number;
  at: string;
  session_id: string | null;
  run_id: string | null;
  payload: { type: string; [k: string]: unknown };
}

export interface ChatMessage {
  id: string;
  role: "user" | "assistant" | "system";
  content: { text?: string; operations?: OperationCard[] };
  operations: string[];
  at: string;
}

export interface OperationCard {
  operation_id: string;
  kind: string;
  display: string;
  result?: { type: string; [k: string]: unknown };
}

export interface ChatReply {
  message_id: string;
  text: string;
  operations: OperationCard[];
  offline: boolean;
}

export interface Command {
  id: string;
  idempotency_key: string;
  actor: "user" | "orchestrator" | "system";
  kind: { type: string; [k: string]: unknown };
}

export interface CommandOutcome {
  type: "applied" | "denied" | "rejected" | "outcome_unknown";
  result?: unknown;
  reason?: string;
}
