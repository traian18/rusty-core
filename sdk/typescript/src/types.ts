/**
 * Wire-protocol types for harnessd protocol v2.
 *
 * These types mirror the Rust types in `crates/harness-protocol/src/{rpc,events,commands,ids,tools}.rs`
 * exactly, including serde's default externally tagged enum representation.
 * This is a hand-authored mirror pending protocol code generation (SDK-201).
 */

export const PROTOCOL_VERSION = 2 as const;

// IDs are UUID strings on the wire.
export type SessionId = string;
export type AgentId = string;
export type RunId = string;
export type RequestId = string;
export type ToolCallId = string;
export type ToolId = string;
export type MessageId = string;
export type EventId = string;
export type PermissionId = string;
export type CommandId = string;
export type Timestamp = string;

export interface Attachment {
  mime_type: string;
  /** Exact serde_json representation of Rust `Vec<u8>`. */
  data: number[];
}

export interface UserInput {
  text: string;
  attachments: Attachment[];
}

export type PermissionDecision = "Approved" | "Denied";

export interface AgentError {
  message: string;
  code: string;
  details?: unknown;
}

export type AgentStatus =
  | "Idle"
  | "PreparingContext"
  | "WaitingForBackend"
  | "Streaming"
  | "Executing"
  | "WaitingForPermission"
  | "WaitingForChildren"
  | "Paused"
  | "Completed"
  | "Cancelled"
  | "Failed";

export interface ToolDescriptor {
  id: ToolId;
  name: string;
  description: string;
  input_schema: unknown;
}

export type PermissionMode = "Allow" | "Ask" | "Deny";

export interface ToolPolicy {
  permission: PermissionMode;
  enabled: boolean;
}

export interface ToolCapability {
  descriptor: ToolDescriptor;
  policy: ToolPolicy;
  delegatable: boolean;
}

/** Rust `HashMap<ToolId, ToolCapability>` serializes as a JSON object. */
export interface AgentToolset {
  tools: Record<ToolId, ToolCapability>;
}

/**
 * How to reach an MCP server. Mirrors
 * `harness_protocol::mcp::McpTransportSpec`.
 */
export type McpTransportSpec =
  | {
      kind: "stdio";
      command: string;
      args?: string[];
      env?: Record<string, string>;
      cwd?: string | null;
    }
  | {
      kind: "http";
      url: string;
      /** Extra request headers — where an `Authorization` goes. */
      headers?: Record<string, string>;
    };

/**
 * Connection spec for one MCP server, connected when the session that
 * requests it starts. Mirrors `harness_protocol::mcp::McpServerSpec`
 * field-for-field — see `crates/harness-protocol/src/mcp.rs`.
 *
 * The flat `command`/`args`/`env`/`cwd` fields are the original shape, from
 * when stdio was the only transport, and still describe a stdio server when
 * `transport` is absent. When `transport` is present it wins and the flat
 * fields are ignored. Prefer setting `transport` on new code.
 */
export interface McpServerSpec {
  name: string;
  transport?: McpTransportSpec | null;
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  cwd?: string | null;
  request_timeout_secs?: number | null;
}

/**
 * Which directories a session scans for `SKILL.md` files. Mirrors
 * `harness_protocol::skills::SkillsSpec` — see
 * `crates/harness-protocol/src/skills.rs`.
 *
 * There is no workspace root here on purpose: `create_session` already
 * carries one, and `include_workspace_dir` selects whether to scan
 * `<that root>/.harness/skills`.
 */
export interface SkillsSpec {
  /**
   * Scan `$HOME/.harness/skills`. Defaults to `false` over the wire — the
   * daemon's home directory is not the caller's, so loading the operator's
   * personal skills is an explicit choice.
   */
  include_user_dir?: boolean;
  /** Scan `<workspace_root>/.harness/skills`. Defaults to `true`. */
  include_workspace_dir?: boolean;
  /** Extra roots, scanned last so they win on a name collision. */
  roots?: string[];
}

export interface ToolCall {
  id: ToolCallId;
  name: string;
  arguments: unknown;
}

export interface ToolProgress {
  status: string;
  fraction: number;
}

export interface ToolResultSummary {
  has_error: boolean;
  output_preview: string;
}

export interface PermissionRequest {
  id: PermissionId;
  tool_call: ToolCall;
  agent_id: AgentId;
}

export interface AgentUsageSnapshot {
  timestamp: string;
  [key: string]: unknown;
}

export type EventVisibility = "User" | "Developer" | "Internal";
export type AgentOutcome = "Success" | "Cancelled" | "Failed";

export type AgentEvent =
  | { StateChanged: { from: AgentStatus; to: AgentStatus } }
  | { RunStarted: { run_id: RunId } }
  | { BackendRequestStarted: { request_id: RequestId } }
  | { AssistantMessageStarted: { message_id: MessageId } }
  | { AssistantTextDelta: { message_id: MessageId; delta: string } }
  | { ReasoningDelta: { message_id: MessageId; delta: string } }
  | { AssistantMessageCompleted: { message_id: MessageId } }
  | { ToolCallRequested: { call: ToolCall } }
  | { ToolCallStarted: { call_id: ToolCallId } }
  | { ToolCallProgress: { call_id: ToolCallId; progress: ToolProgress } }
  | { ToolCallCompleted: { call_id: ToolCallId; result: ToolResultSummary } }
  | { PermissionRequested: { request: PermissionRequest } }
  | { UsageUpdated: { usage: AgentUsageSnapshot } }
  | { ChildAgentSpawned: { agent_id: AgentId } }
  | { ChildAgentCompleted: { agent_id: AgentId; outcome: AgentOutcome } }
  | { Failed: { error: AgentError } }
  | { Completed: { outcome: AgentOutcome } };

export interface AgentEventEnvelope {
  event_id: EventId;
  session_id: SessionId;
  agent_id: AgentId;
  parent_agent_id: AgentId | null;
  run_id: RunId | null;
  agent_sequence: number;
  session_sequence: number | null;
  timestamp: Timestamp;
  visibility: EventVisibility;
  event: AgentEvent;
}

export interface ProtocolCapabilities {
  resumable_subscribe: boolean;
  lifecycle_commands: boolean;
  typed_errors: boolean;
  mutation_admission: boolean;
  session_restore: boolean;
  event_gap_signals: boolean;
  durable_idempotency: boolean;
  pause_resume: boolean;
}

export interface MutationMetadata {
  command_id: CommandId;
  session_id: SessionId;
  run_id: RunId | null;
  expected_session_revision: number | null;
  trace_id: string | null;
}

export type MutationCommand =
  | { type: "prompt"; payload: UserInput }
  | { type: "steer"; payload: UserInput }
  | { type: "follow_up"; payload: UserInput }
  | { type: "cancel" }
  | {
      type: "resolve_permission";
      payload: { id: PermissionId; decision: PermissionDecision };
    }
  | { type: "close_session" };

export type AdmissionResult =
  | { type: "accepted" }
  | { type: "accepted_started"; payload: { run_id: RunId } }
  | {
      type: "accepted_queued";
      payload: { run_id: RunId; position: number };
    }
  | { type: "accepted_applied" }
  | { type: "duplicate"; payload: { original: AdmissionResult } }
  | {
      type: "rejected_conflict";
      payload: { current_session_revision: number };
    }
  | { type: "rejected_closed" }
  | { type: "rejected_invalid_state"; payload: { reason: string } }
  | { type: "rejected_capacity"; payload: { limit: string } };

export type RpcErrorCategory =
  | "protocol"
  | "validation"
  | "not_found"
  | "conflict"
  | "capacity"
  | "lifecycle"
  | "persistence"
  | "integration"
  | "internal";

export interface RpcErrorPayload {
  code: string;
  category: RpcErrorCategory;
  retryable: boolean;
  message: string;
  details?: unknown;
  trace_id?: string;
  run_id?: RunId;
  provider_request_id?: string;
}

export interface SessionSummaryWire {
  session_id: SessionId;
  title: string;
  backend_name: string | null;
  updated_at: Timestamp;
  restorable: boolean;
}

export type RpcRequestBody =
  | { type: "hello"; payload: { protocol_version: number } }
  | {
      type: "create_session";
      payload: {
        workspace_root: string;
        integration: string;
        integration_config: unknown;
        toolset: AgentToolset;
        mcp_servers?: McpServerSpec[];
        skills?: SkillsSpec | null;
      };
    }
  | {
      type: "mutate";
      payload: { metadata: MutationMetadata; command: MutationCommand };
    }
  | { type: "list_sessions" }
  | {
      type: "restore_session";
      payload: {
        session_id: SessionId;
        workspace_root: string;
        toolset: AgentToolset;
      };
    }
  | { type: "snapshot" }
  | { type: "subscribe"; payload: { since_seq: number | null } };

export interface RpcRequest {
  id: number;
  session_id: SessionId | null;
  body: RpcRequestBody;
}

export type SessionSnapshotWire = Record<string, unknown>;

export type RpcResponseBody =
  | {
      type: "hello";
      payload: {
        protocol_version: number;
        capabilities: ProtocolCapabilities;
      };
    }
  | { type: "session_created"; payload: { session_id: SessionId } }
  | { type: "session_restored"; payload: { session_id: SessionId; session_revision: number } }
  | { type: "sessions_listed"; payload: { sessions: SessionSummaryWire[] } }
  | {
      type: "admission";
      payload: {
        metadata: MutationMetadata;
        result: AdmissionResult;
        session_revision: number;
      };
    }
  | { type: "snapshot"; payload: SessionSnapshotWire }
  | { type: "event"; payload: AgentEventEnvelope }
  | {
      type: "event_gap";
      payload: {
        session_id: SessionId;
        last_delivered_sequence: number;
        dropped: number;
        cursor_expired: boolean;
      };
    }
  | { type: "ack" }

  | { type: "failure"; payload: RpcErrorPayload };

export interface RpcResponse {
  id: number | null;
  body: RpcResponseBody;
}

export function isKnownRpcResponseBody(body: RpcResponseBody): body is RpcResponseBody {
  return [
    "hello",
    "session_created",
    "session_restored",
    "sessions_listed",
    "admission",
    "snapshot",
    "event",
    "event_gap",
    "ack",

    "failure",
  ].includes(body.type);
}
