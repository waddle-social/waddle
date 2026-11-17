import type { XmppStanzaErrorType } from "./types";

/**
 * A room join that did not complete: the room (or the home server's router
 * on its behalf) answered with an error presence, or no self-presence came
 * within the join budget (`timedOut`). Carries the stanza error so callers
 * can tell an outage from a denial or a missing room.
 */
export class RoomJoinError extends Error {
  readonly roomJid: string;
  readonly condition?: string;
  readonly errorType?: XmppStanzaErrorType;
  readonly timedOut: boolean;
  /** The join created the room: it did not exist on its server. */
  readonly roomCreated: boolean;

  constructor(
    message: string,
    details: {
      roomJid: string;
      condition?: string | null;
      errorType?: XmppStanzaErrorType | null;
      timedOut?: boolean;
      roomCreated?: boolean;
    },
  ) {
    super(message);
    this.name = "RoomJoinError";
    this.roomJid = details.roomJid;
    if (details.condition) this.condition = details.condition;
    if (details.errorType) this.errorType = details.errorType;
    this.timedOut = details.timedOut ?? false;
    this.roomCreated = details.roomCreated ?? false;
  }
}
