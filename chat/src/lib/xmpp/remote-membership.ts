// How a user's relationship with a remote room or Space ends.
//
// Four user actions and one host decision each have their own record to
// end, and none removes a record another one owns, so none can be mistaken
// for another:
//
// | End               | Room occupancy | XEP-0402 bookmark | Space subscription | Joined-space entry | Host grant |
// | ----------------- | -------------- | ----------------- | ------------------ | ------------------ | ---------- |
// | leave-room        | ends           | autojoin off      | kept               | kept               | kept       |
// | forget-room       | kept           | removed           | kept               | kept               | kept       |
// | unsubscribe-space | kept           | kept              | ends               | follow off         | kept       |
// | forget-space      | kept           | kept              | kept               | removed            | kept       |
// | revoked           | ends           | kept              | kept               | kept               | removed    |
//
// Leaving also turns the bookmark's `autojoin` off, so the room is not
// joined again at the next sign-in; the bookmark itself stays. `revoked` is
// the host's decision (a ban, or a removed membership): the occupancy ends
// with it, the client keeps every record of its own and does not join again
// in the session. A bookmark that joins at sign-in is tried once at the
// next sign-in, where the host's refusal (`forbidden` or
// `registration-required`) blocks further automatic joins.
// Forgetting is offered only once nothing is live (the room left, the
// Space no longer followed), so a forgotten community never keeps
// delivering traffic nothing shows.

type MembershipAction = "leave-room" | "forget-room" | "unsubscribe-space" | "forget-space";
export type MembershipEnd = MembershipAction | "revoked";

/** The record an end exists to change. */
type MembershipRecord = "occupancy" | "bookmark" | "subscription" | "preference" | "hostGrant";

export interface MembershipEffect {
  primary: MembershipRecord;
  occupancy: "ends" | "kept";
  bookmark: "autojoin-off" | "removed" | "kept";
  subscription: "ends" | "kept";
  preference: "follow-off" | "removed" | "kept";
  hostGrant: "removed" | "kept";
  /** Whether the client joins or subscribes again on its own in the same session. */
  automaticRetry: boolean;
}

export const MEMBERSHIP_EFFECTS: Readonly<Record<MembershipEnd, MembershipEffect>> = {
  "leave-room": {
    primary: "occupancy",
    occupancy: "ends",
    bookmark: "autojoin-off",
    subscription: "kept",
    preference: "kept",
    hostGrant: "kept",
    automaticRetry: false,
  },
  "forget-room": {
    primary: "bookmark",
    occupancy: "kept",
    bookmark: "removed",
    subscription: "kept",
    preference: "kept",
    hostGrant: "kept",
    automaticRetry: false,
  },
  "unsubscribe-space": {
    primary: "subscription",
    occupancy: "kept",
    bookmark: "kept",
    subscription: "ends",
    preference: "follow-off",
    hostGrant: "kept",
    automaticRetry: false,
  },
  "forget-space": {
    primary: "preference",
    occupancy: "kept",
    bookmark: "kept",
    subscription: "kept",
    preference: "removed",
    hostGrant: "kept",
    automaticRetry: false,
  },
  revoked: {
    primary: "hostGrant",
    occupancy: "ends",
    bookmark: "kept",
    subscription: "kept",
    preference: "kept",
    hostGrant: "removed",
    automaticRetry: false,
  },
};

export type RoomAction = "join" | "leave-room" | "forget-room";
export type SpaceAction = "follow" | "unsubscribe-space" | "forget-space";

/** The actions offered for a room the user has joined and/or bookmarked. */
export function roomActions(state: { joined: boolean; bookmarked: boolean }): RoomAction[] {
  if (state.joined) return ["leave-room"];
  return state.bookmarked ? ["join", "forget-room"] : ["join"];
}

/** The actions offered for a Space the user lists and/or follows. */
export function spaceActions(state: { following: boolean; listed: boolean }): SpaceAction[] {
  if (state.following) return ["unsubscribe-space"];
  return state.listed ? ["follow", "forget-space"] : ["follow"];
}

/** Why the room ended the user's own occupancy (XEP-0045 §7.14, §9, §10, §11). */
export type RoomDeparture = "left" | "revoked" | "kicked" | "outage";

/**
 * Read the status codes of the user's own unavailable presence:
 * 301 (banned), 321 (affiliation removed) and 322 (room became members-only)
 * are revocations; 307 is a kick, after which a manual rejoin may succeed;
 * 332 (service shutdown) and 333 (technical reasons, such as a lost
 * federation stream) are outages after which joining again is expected.
 */
export function classifyOwnDeparture(statusCodes: readonly number[]): RoomDeparture {
  if (statusCodes.includes(301) || statusCodes.includes(321) || statusCodes.includes(322)) return "revoked";
  if (statusCodes.includes(307)) return "kicked";
  if (statusCodes.includes(332) || statusCodes.includes(333)) return "outage";
  return "left";
}
