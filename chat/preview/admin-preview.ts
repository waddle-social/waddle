import { connectionStore } from "@/lib/connection-store";
import { navigate, type AdminPanel } from "@/router";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { ChatAppController } from "@/shell/chat-app-controller";

/**
 * Preview fixtures for the admin screens:
 *   admin-users | admin-spaces | admin-channels | admin-audit (stub)
 *   admin-denied | admin-error | admin-checking
 * A fake client answers the admin commands; every other call stays pending.
 */
const D = "waddle.test";
const users = [
  ["jo", "Jo Hartmann", true], ["mika", "Mika"], ["ana", "Ana Lindqvist"], ["priya", "Priya"], ["kim", ""],
].map(([l, n, owner]) => ({ jid: `${l}@${D}`, display_name: (n as string) || null, has_owner_hat: !!owner }));
const spaces = [
  ["waddle-hq", "Waddle HQ", 8, 24, "Where the team ships Waddle."], ["xsr", "XMPP Standards Readers", 2, 9], ["penguin-club", "Penguin Club", 1, 3],
].map(([node, name, ch, mem, description]) => ({
  space_jid: `spaces.${D}`, space_node: node as string, name: name as string, description: (description as string) ?? null,
  channel_count: ch as number, member_count: mem as number,
}));
const channels = [
  ["release-train", "text", true, false, 14, "Shipping 0.9 together"], ["announcements", "announcement", true, false, 24],
  ["design-critique", "forum", true, true, 11], ["moderators", "text", false, true, 3],
].map(([name, type, pub, mo, mem, topic]) => ({
  channel_jid: `${name}@muc.${D}`, name: name as string, topic: (topic as string) ?? null, channel_type: type,
  is_public: pub as boolean, members_only: mo as boolean, occupant_count: 4, owner_count: 1, admin_count: 1,
  member_count: mem as number, outcast_count: 1,
}));
const page = <T>(entries: T[], next = false) => Promise.resolve({ entries, next_cursor: next ? "c1" : null });
const pending = () => new Promise(() => {});

function fakeClient(screen: string): BrowserXmppClient {
  const api: Record<string, (...args: unknown[]) => unknown> = {
    isCommunityOwner: () => screen === "admin-checking" ? pending()
      : screen === "admin-error" ? Promise.reject(new Error("timeout"))
      : Promise.resolve(screen !== "admin-denied"),
    adminUsersList: () => page(users, true),
    adminSpacesList: () => page(spaces),
    adminSpacesMembers: () => page([["jo", "owner"], ["mika", "admin"], ["priya", "member"], ["theo", "member"]].map(([l, role]) => ({ jid: `${l}@${D}`, role })), true),
    adminChannelsList: () => page(channels),
    adminChannelsAffiliations: () => page([["jo", "owner"], ["mika", "admin"], ["ana", "member"], ["priya", "member"], ["spam-bot", "outcast", "Link spam"]]
      .map(([l, affiliation, reason]) => ({ jid: `${l}@${l === "spam-bot" ? "example.net" : D}`, affiliation, reason: reason ?? null }))),
    adminChannelsOccupants: () => page([["Mika", "mika", "moderator", "admin"], ["Ana", "ana", "participant", "member"], ["Priya", "priya", "participant", "member"], ["Theo", "theo", "visitor", "none"]]
      .map(([nick, l, role, affiliation]) => ({ nick, real_jid: `${l}@${D}`, role, affiliation }))),
  };
  return new Proxy(api, { get: (t, k) => (typeof k === "string" && k in t ? t[k] : pending) }) as unknown as BrowserXmppClient;
}

export function seedAdmin(c: ChatAppController, screen: string): void {
  if (!screen.startsWith("admin")) return;
  const panel = (["users", "spaces", "channels", "audit"].find((p) => screen === `admin-${p}`) ?? "users") as AdminPanel;
  connectionStore.client = fakeClient(screen);
  navigate({ id: "admin", params: { panel } }, { replace: true });
  c.ui.activePage.value = "admin";
}
