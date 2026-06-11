import type { CallMedia } from "./types";

/**
 * The subset of the wasm `WaddleClient` API used to originate calls.
 * Every method is `?`-optional so unit tests can stub a partial
 * client, and so the chat layer remains tolerant of older wasm
 * builds where `send_call_*` is not yet present.
 */
export type CallWireSender = {
  send_call_propose?: (peer_bare_jid: string, sid: string, audio: boolean, video: boolean) => Promise<unknown>;
  send_call_proceed?: (peer_full_jid: string, sid: string) => Promise<unknown>;
  send_call_ringing?: (peer_bare_jid: string, sid: string) => Promise<unknown>;
  send_call_reject?: (peer_full_jid: string, sid: string) => Promise<unknown>;
  send_call_reject_tie_break?: (peer_full_jid: string, sid: string) => Promise<unknown>;
  send_call_retract?: (peer_bare_jid: string, sid: string) => Promise<unknown>;
  send_call_retract_tie_break?: (peer_full_jid: string, sid: string) => Promise<unknown>;
  send_call_finish?: (peer_full_jid: string, sid: string) => Promise<unknown>;
  send_call_finish_migrated?: (peer_full_jid: string, old_sid: string, new_sid: string) => Promise<unknown>;
  send_call_session_initiate?: (
    peer_full_jid: string,
    initiator_full_jid: string,
    sid: string,
    audio: boolean,
    video: boolean,
  ) => Promise<unknown>;
  send_call_session_accept?: (
    peer_full_jid: string,
    responder_full_jid: string,
    sid: string,
    audio: boolean,
    video: boolean,
  ) => Promise<unknown>;
  send_call_session_terminate?: (
    peer_full_jid: string,
    sid: string,
    reason: string | null,
  ) => Promise<unknown>;
  send_call_session_terminate_with_outcome?: (
    peer_full_jid: string,
    sid: string,
    reason: string | null,
  ) => Promise<unknown>;
};

export type CallSessionTerminateOutcome = "ok" | "orphaned" | "error";

/**
 * Generate a fresh Jingle session id. The server namespaces this
 * with the caller's bare JID before scoping a LiveKit room, so two
 * clients picking the same `sid` only collides within a single
 * caller's own active calls — which the local UI prevents anyway.
 */
export function newCallSid(): string {
  return `c${crypto.randomUUID().replaceAll("-", "").slice(0, 16)}`;
}

class WasmCallApiUnavailable extends Error {
  constructor(method: keyof CallWireSender) {
    super(`wasm client does not expose ${method}; rebuild the wasm bundle`);
    this.name = "WasmCallApiUnavailable";
  }
}

/**
 * Typed wrappers around the wasm `send_call_*` methods. Each wrapper
 * throws `WasmCallApiUnavailable` if the underlying method is
 * missing so callers can render a clear error rather than silently
 * dropping the action.
 */
export const outboundCalls = {
  async propose(
    client: CallWireSender,
    peerBareJid: string,
    sid: string,
    media: CallMedia,
  ): Promise<void> {
    if (!client.send_call_propose) throw new WasmCallApiUnavailable("send_call_propose");
    await client.send_call_propose(peerBareJid, sid, media.audio, media.video);
  },

  async proceed(client: CallWireSender, peerFullJid: string, sid: string): Promise<void> {
    if (!client.send_call_proceed) throw new WasmCallApiUnavailable("send_call_proceed");
    await client.send_call_proceed(peerFullJid, sid);
  },

  async ringing(client: CallWireSender, peerBareJid: string, sid: string): Promise<void> {
    if (!client.send_call_ringing) throw new WasmCallApiUnavailable("send_call_ringing");
    await client.send_call_ringing(peerBareJid, sid);
  },

  async reject(client: CallWireSender, peerFullJid: string, sid: string): Promise<void> {
    if (!client.send_call_reject) throw new WasmCallApiUnavailable("send_call_reject");
    await client.send_call_reject(peerFullJid, sid);
  },

  async rejectTieBreak(client: CallWireSender, peerFullJid: string, sid: string): Promise<void> {
    if (!client.send_call_reject_tie_break)
      throw new WasmCallApiUnavailable("send_call_reject_tie_break");
    await client.send_call_reject_tie_break(peerFullJid, sid);
  },

  async retract(client: CallWireSender, peerBareJid: string, sid: string): Promise<void> {
    if (!client.send_call_retract) throw new WasmCallApiUnavailable("send_call_retract");
    await client.send_call_retract(peerBareJid, sid);
  },

  async retractTieBreak(client: CallWireSender, peerFullJid: string, sid: string): Promise<void> {
    if (!client.send_call_retract_tie_break)
      throw new WasmCallApiUnavailable("send_call_retract_tie_break");
    await client.send_call_retract_tie_break(peerFullJid, sid);
  },

  async finish(client: CallWireSender, peerFullJid: string, sid: string): Promise<void> {
    if (!client.send_call_finish) throw new WasmCallApiUnavailable("send_call_finish");
    await client.send_call_finish(peerFullJid, sid);
  },

  async finishMigrated(
    client: CallWireSender,
    peerFullJid: string,
    oldSid: string,
    newSid: string,
  ): Promise<void> {
    if (!client.send_call_finish_migrated)
      throw new WasmCallApiUnavailable("send_call_finish_migrated");
    await client.send_call_finish_migrated(peerFullJid, oldSid, newSid);
  },

  async sessionInitiate(
    client: CallWireSender,
    peerFullJid: string,
    initiatorFullJid: string,
    sid: string,
    media: CallMedia,
  ): Promise<void> {
    if (!client.send_call_session_initiate)
      throw new WasmCallApiUnavailable("send_call_session_initiate");
    await client.send_call_session_initiate(peerFullJid, initiatorFullJid, sid, media.audio, media.video);
  },

  async sessionAccept(
    client: CallWireSender,
    peerFullJid: string,
    responderFullJid: string,
    sid: string,
    media: CallMedia,
  ): Promise<void> {
    if (!client.send_call_session_accept)
      throw new WasmCallApiUnavailable("send_call_session_accept");
    await client.send_call_session_accept(
      peerFullJid,
      responderFullJid,
      sid,
      media.audio,
      media.video,
    );
  },

  async sessionTerminate(
    client: CallWireSender,
    peerFullJid: string,
    sid: string,
    reason: string | null,
  ): Promise<void> {
    if (!client.send_call_session_terminate)
      throw new WasmCallApiUnavailable("send_call_session_terminate");
    await client.send_call_session_terminate(peerFullJid, sid, reason);
  },

  async sessionTerminateWithOutcome(
    client: CallWireSender,
    peerFullJid: string,
    sid: string,
    reason: string | null,
  ): Promise<CallSessionTerminateOutcome> {
    if (!client.send_call_session_terminate_with_outcome) {
      await this.sessionTerminate(client, peerFullJid, sid, reason);
      return "ok";
    }
    const result = await client.send_call_session_terminate_with_outcome(peerFullJid, sid, reason);
    if (isCallSessionTerminateOutcome(result)) return result.kind;
    return "error";
  },
};

function isCallSessionTerminateOutcome(value: unknown): value is { kind: CallSessionTerminateOutcome } {
  if (!value || typeof value !== "object") return false;
  const kind = (value as { kind?: unknown }).kind;
  return kind === "ok" || kind === "orphaned" || kind === "error";
}
