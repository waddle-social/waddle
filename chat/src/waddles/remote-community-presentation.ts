import type { CommunityAddressError } from "@/lib/xmpp/remote-address";
import { describeRemoteFailure, type RemoteSubject } from "@/lib/xmpp/remote-outcome";
import type { RemoteRoomFlags } from "@/lib/xmpp/remote-community";
import type { RoomDeparture } from "@/lib/xmpp/remote-membership";
import type { RemoteStatus } from "@/waddles/remote-communities";

type StatusTone = "neutral" | "progress" | "warning" | "danger";

export interface StatusLine {
  tone: StatusTone;
  title: string;
  detail: string | null;
  /** A retry button is offered. */
  canRetry: boolean;
  /** Seconds until an automatic retry, when one is scheduled. */
  retryInSeconds: number | null;
}

export function addressErrorMessage(error: CommunityAddressError): string {
  switch (error) {
    case "empty":
      return "Enter the address of a room, a Space or a server.";
    case "resource":
      return "Use the room's address without a nickname: room@server, not room@server/name.";
    case "refused-domain":
      return "Other servers are reached by their domain name. IP addresses and single-word names can't be used.";
    default:
      return "That doesn't look like a room, Space or server address.";
  }
}

/**
 * What to show for an entry's status. Outages, denials and missing
 * communities read differently, and only outages count down to an
 * automatic retry.
 */
export function statusLine(status: RemoteStatus, subject: RemoteSubject, nowMs: number): StatusLine | null {
  if (status.state === "idle" || status.state === "ready") return null;
  if (status.state === "working") {
    return { tone: "progress", title: "Asking its server…", detail: null, canRetry: false, retryInSeconds: null };
  }
  const { title, detail } = describeRemoteFailure(status.failure, subject);
  const tone: StatusTone = status.failure.kind === "outage" ? "warning" : "danger";
  return {
    tone,
    title,
    detail,
    canRetry: status.failure.retry !== "none",
    retryInSeconds: status.retryAt === null ? null : Math.max(0, Math.ceil((status.retryAt - nowMs) / 1000)),
  };
}

/** A note about how the room last ended the user's occupancy. */
export function departureNote(departure: RoomDeparture | null): string | null {
  switch (departure) {
    case "revoked":
      return "The room removed your access. Your bookmark is kept; joining again needs an admin there.";
    case "kicked":
      return "You were removed from the room. You can try to join again.";
    case "outage":
      return "The connection to the room's server was lost.";
    case "destroyed":
      return "The room was closed on its server. Your bookmark is kept until you forget it.";
    default:
      return null;
  }
}

/** Labels for what a room says about itself. */
export function roomFlagLabels(flags: RemoteRoomFlags): string[] {
  const labels: string[] = [];
  if (flags.membersOnly) labels.push("Members only");
  if (flags.passwordProtected) labels.push("Needs a password");
  if (flags.moderated) labels.push("Moderated");
  if (flags.hidden) labels.push("Not listed");
  if (flags.nonAnonymous) labels.push("Everyone there sees your address");
  return labels;
}

/** Initial shown in place of an image the client does not fetch. */
export function placeholderInitial(name: string | null, fallback: string): string {
  const source = (name ?? fallback).trim();
  return source ? Array.from(source)[0]!.toUpperCase() : "?";
}

/**
 * How to name a member: by username when the account is on this
 * deployment, by full address otherwise, so an account elsewhere is never
 * mistaken for the local account with the same name.
 */
export function memberIdentityLabel(member: { jid: string; username: string }, homeDomain: string | null | undefined): string {
  const bare = member.jid.split("/")[0] ?? member.jid;
  const domain = bare.slice(bare.lastIndexOf("@") + 1).toLowerCase();
  const home = homeDomain?.trim().toLowerCase();
  return !home || domain === home ? member.username : bare;
}
