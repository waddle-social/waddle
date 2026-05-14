/**
 * Typed inbound call event surfaced by the WASM client's `on_call`
 * callback. Mirrors `WaddleCallEvent` produced by
 * `server/crates/waddle-xmpp-client-wasm/src/types.rs`.
 *
 * The `kind` discriminator drives a switch on the chat side; see
 * `CallEvent` for the exhaustive variant list and which fields are
 * populated per variant.
 */
export type CallMedia = { audio: boolean; video: boolean };

export type LiveKitJoin = {
  url: string;
  room: string;
  identity: string;
  token: string;
};

export type CallEvent =
  | { kind: "propose"; from: string; sid: string; media: CallMedia }
  | { kind: "proceed"; from: string; sid: string }
  | { kind: "reject"; from: string; sid: string }
  | { kind: "retract"; from: string; sid: string }
  | { kind: "finish"; from: string; sid: string }
  | {
      kind: "session-initiate";
      from: string;
      sid: string;
      media: CallMedia;
      join: LiveKitJoin;
    }
  | {
      kind: "session-accept";
      from: string;
      sid: string;
      media: CallMedia;
      join: LiveKitJoin;
    }
  | { kind: "session-terminate"; from: string; sid: string; reason: string | null };

/**
 * The chat-side reduced state representing the lifecycle of a
 * single call slot. The current implementation tracks one call at
 * a time; if concurrent calls become a requirement later, switch
 * the store from an atom to a map keyed by `sid`.
 */
export type CallState =
  | { phase: "idle" }
  | { phase: "incoming"; from: string; sid: string; media: CallMedia }
  | { phase: "outgoing"; to: string; sid: string; media: CallMedia }
  | {
      phase: "active";
      peer: string;
      sid: string;
      media: CallMedia;
      join: LiveKitJoin;
    }
  | { phase: "ended"; sid: string; reason: string | null };
