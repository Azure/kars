// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Message types for federation protocol between external agents and controller.
 */

// ---------------------------------------------------------------------------
// Pairing
// ---------------------------------------------------------------------------

export interface PairRequestMessage {
  type: "pair_request";
  secret: string;
  pubkey_ed25519: string;
  pubkey_x25519?: string;
  display_name?: string;
  capabilities_requested?: string[];
}

export interface PairResponseMessage {
  type: "pair_response";
  success: boolean;
  cluster_name?: string;
  controller_amid?: string;
  capabilities_granted?: string[];
  slots?: number;
  token_budget?: number;
  expires_at?: string;
  error?: string;
}

// ---------------------------------------------------------------------------
// File content (inline transfer)
// ---------------------------------------------------------------------------

export interface FileContent {
  path: string;
  data_b64: string;
  size: number;
}

// ---------------------------------------------------------------------------
// Task Offload
// ---------------------------------------------------------------------------

export interface OffloadRequestMessage {
  type: "offload_request";
  task: string;
  files: string[];
  file_count: number;
  total_bytes: number;
  file_contents?: FileContent[];
  preferences?: {
    model?: string;
    max_tokens?: number;
    timeout_minutes?: number;
  };
  request_id: string;
  timestamp: string;
}

export interface OffloadStatusMessage {
  type: "offload_status";
  request_id: string;
  phase: "validating" | "spawning" | "scheduled" | "ready" | "running" | "returning" | "done" | "error";
  message: string;
  /** Set when phase === "ready" — the sandbox name to discover on the mesh */
  sandbox_name?: string;
}

export interface OffloadProgressMessage {
  type: "offload_progress";
  request_id: string;
  stage: string;
  pct: number;
  message: string;
}

export interface OffloadDoneMessage {
  type: "offload_done";
  request_id: string;
  summary: string;
  output_files: string[];
  output_file_contents?: FileContent[];
  tokens_used: { prompt: number; completion: number };
  duration_seconds: number;
}

export interface OffloadErrorMessage {
  type: "offload_error";
  request_id: string;
  error: string;
  phase: string;
}

// ---------------------------------------------------------------------------
// Cloud Handoff
// ---------------------------------------------------------------------------

export interface HandoffRequestMessage {
  type: "handoff_request";
  mode: "cloud";
  direction: "local_to_cloud" | "cloud_to_local";
  state_size_bytes: number;
  request_id: string;
}

export interface HandoffRecallMessage {
  type: "handoff_recall";
  request_id: string;
}

export interface HandoffProgressMessage {
  type: "handoff_progress";
  request_id: string;
  round: number;
  summary: string;
}

// ---------------------------------------------------------------------------
// Union type
// ---------------------------------------------------------------------------

export type FederationMessage =
  | PairRequestMessage
  | PairResponseMessage
  | OffloadRequestMessage
  | OffloadStatusMessage
  | OffloadProgressMessage
  | OffloadDoneMessage
  | OffloadErrorMessage
  | HandoffRequestMessage
  | HandoffRecallMessage
  | HandoffProgressMessage;
