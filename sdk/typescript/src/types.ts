/**
 * Wire-protocol types for harnessd protocol v1.
 *
 * These types mirror the Rust types in `crates/harness-protocol/src/{rpc,events,commands,ids}.rs`
 * exactly, including serde's default "externally tagged" enum representation:
 *
 * - A unit variant (e.g. `Cancel`) serializes as the bare string `"Cancel"`.
 * - A struct/tuple variant (e.g. `Prompt(UserInput)`) serializes as
 *   `{ "Prompt": { ... } }` — exactly one key, named after the variant.
 *
 * This is a hand-authored mirror of the Rust source, not a generated
 * artifact. See `schema/protocol-v1.schema.json` for the language-neutral
 * JSON Schema description of the same shapes, and `sdk_plan.md` (SDK-201)
 * for the plan to replace both with generated code from one source of
 * truth.
 *
 * IMPORTANT: serde's default enum representation is "closed" — an
 * unrecognized variant fails to deserialize on the Rust side. Until the
 * protocol adopts an explicit `#[serde(tag = "type")]` + catch-all
 * representation (tracked in `sdk_plan.md` SDK-200), do not assume unknown
 * additive variants are safely ignorable by this client either; guard with
 * {@link isKnownRpcResponseBody} before matching.
 */

export const PROTOCOL_VERSION = 1 as const;

// ---------------------------------------------------------------------------
// IDs
// ---------------------------------------------------------------------------

/** All harness identifiers are plain UUID strings on the wire. */
export type SessionId = string;
export type AgentId = string;
export type RunId = string;
export type RequestId = string;
export type ToolCallId = string;
export type MessageId = string;
export type EventId = string;
export type PermissionId = string;

/** RFC3339 UTC timestamp string, as produced by `chrono`'s serde impl. */
export type Timestamp = string;

// ---------------------------------------------------------------------------
// Commands / user input
// ---------------------------------------------------------------------------

export interface Attachment {
  mime_type: string;
  /** Base64-encoded bytes (JSON has no native byte-array type). */
  data: string;
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

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

export interface AgentUsageSnapshot {
  timestamp: string;
  [key: string]: unknown;
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// RPC request/response envelopes
// ---------------------------------------------------------------------------

export interface ProtocolCapabilities {
  resumable_subscribe: boolean;
}

export type RpcRequestBody =
  | { Hello: { protocol_version: number } }
  | {
      CreateSession: {
        workspace_root: string;
        integration: string;
        integration_config: unknown;
        toolset: string[];
      };
    }
  | { Prompt: UserInput }
  | "Cancel"
  | "Pause"
  | "Resume"
  | { ResolvePermission: { id: PermissionId; decision: PermissionDecision } }
  | "Snapshot"
  | { Subscribe: { since_seq: number | null } }
  | "CloseSession";

export interface RpcRequest {
  /** Client-assigned correlation id, echoed back on the matching response. */
  id: number;
  session_id: SessionId | null;
  body: RpcRequestBody;
}

/**
 * Opaque wire representation of a point-in-time session snapshot. Treat as
 * unstructured pending a stable, documented shape (`sdk_plan.md` SDK-400).
 */
export type SessionSnapshotWire = Record<string, unknown>;

export type RpcResponseBody =
  | { Hello: { protocol_version: number; capabilities: ProtocolCapabilities } }
  | { SessionCreated: { session_id: SessionId } }
  | "Ack"
  | { Snapshot: SessionSnapshotWire }
  | { Event: AgentEventEnvelope }
  | { Error: { message: string } };

export interface RpcResponse {
  /** `null` marks a server-pushed event frame, not a reply to a request. */
  id: number | null;
  body: RpcResponseBody;
}

/**
 * Narrow a decoded {@link RpcResponseBody} to a known variant name.
 *
 * Defensive helper for the "closed enum" forward-compatibility gap noted at
 * the top of this file: prefer this over an unchecked `in` check so a
 * future additive variant fails loudly instead of silently mismatching.
 */
export function isKnownRpcResponseBody(
  body: RpcResponseBody,
): body is RpcResponseBody {
  if (typeof body === "string") {
    return body === "Ack";
  }
  const key = Object.keys(body)[0];
  return (
    key === "Hello" ||
    key === "SessionCreated" ||
    key === "Snapshot" ||
    key === "Event" ||
    key === "Error"
  );
}
