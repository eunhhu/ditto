export type EffectClass =
  | "pure"
  | "read"
  | "write-reversible"
  | "write-irreversible"
  | "external-communication"
  | "privileged"
  | "credential-access";

export type ClientCommand =
  | {
      type: "submit";
      input: string;
      session_id?: string | null;
      task_id?: string | null;
    }
  | {
      type: "replay";
      session_id?: string | null;
      task_id?: string | null;
      after_seq?: number;
    }
  | { type: "cancel"; task_id: string }
  | { type: "ping" };

export interface ContextReceiptItem {
  node_id: string;
  label: string;
  source: string;
  epistemic: string;
  reason: string;
}

export interface ContextReceipt {
  capsule: string;
  included: ContextReceiptItem[];
  token_estimate: number;
}

export interface CapabilityCard {
  id: string;
  summary: string;
  namespace: string;
  maximum_effect: EffectClass;
  placements: string[];
}

export type ServerMessage =
  | { type: "accepted"; session_id: string; task_id: string }
  | { type: "context_receipt"; receipt: ContextReceipt }
  | { type: "capabilities_selected"; capabilities: CapabilityCard[] }
  | { type: "event"; event: unknown }
  | { type: "model_delta"; text: string }
  | { type: "end"; verified: boolean }
  | { type: "error"; message: string }
  | { type: "pong" };

export function encodeCommand(command: ClientCommand): string {
  return `${JSON.stringify(command)}\n`;
}

export function decodeMessage(line: string): ServerMessage {
  const value: unknown = JSON.parse(line);
  if (
    typeof value !== "object" ||
    value === null ||
    !("type" in value) ||
    typeof value.type !== "string"
  ) {
    throw new TypeError("invalid Ditto server message");
  }
  return value as ServerMessage;
}
