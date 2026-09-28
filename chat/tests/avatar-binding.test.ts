import { describe, expect, test } from "bun:test";
import { AvatarStore } from "../src/lib/avatars/avatar-store";
import { OccupantJidDirectory } from "../src/lib/avatars/author-jid";
import { createAvatarBinding, type AvatarClient } from "../src/lib/avatars/bind-client";
import type { AvatarChangedEvent } from "../src/lib/xmpp/client-events";

/** Client double with a real multi-listener bus: unsubscribe really unsubscribes. */
function fakeClient() {
  const changed = new Set<(event: AvatarChangedEvent) => void>();
  const occupants = new Set<(roomJid: string, nick: string, bareJid: string) => void>();
  const ownProfile = new Set<(ownBareJid: string) => void>();
  const forgotten: string[] = [];
  const on = <T>(set: Set<T>, handler: T) => {
    set.add(handler);
    return () => { set.delete(handler); };
  };
  const client: AvatarClient = {
    fetchUserAvatar: async (jid) => `data:${jid}`,
    forgetUserAvatar: (jid) => { forgotten.push(jid); },
    addAvatarChangedHandler: (handler) => on(changed, handler),
    addOccupantRealJidHandler: (handler) => on(occupants, handler),
    addOwnProfilePublishedHandler: (handler) => on(ownProfile, handler),
    addOwnOccupantNickHandler: () => () => undefined,
    onStatus: () => () => undefined,
  };
  return {
    client,
    forgotten,
    emitOccupant: (roomJid: string, nick: string, bareJid: string) => { for (const h of occupants) h(roomJid, nick, bareJid); },
    emitAvatarChanged: (event: AvatarChangedEvent) => { for (const h of changed) h(event); },
  };
}

const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

describe("avatar binding", () => {
  test("logout unbinds the old client before resetting, so its late events cannot leak into the next account", async () => {
    const store = new AvatarStore();
    const directory = new OccupantJidDirectory();
    const binding = createAvatarBinding(store, directory);
    const userA = fakeClient();
    binding.bind(userA.client);
    userA.emitOccupant("room@muc.example.com", "sam", "alice@example.com");
    store.retain("alice@example.com");
    await flush();
    expect(directory.lookup("room@muc.example.com", "sam")).toBe("alice@example.com");
    expect(store.urlFor("alice@example.com")).toBe("data:alice@example.com");

    binding.logout();
    // Late events from user A's still-alive client after logout.
    userA.emitOccupant("room@muc.example.com", "sam", "alice@example.com");
    userA.emitAvatarChanged({ jid: "carol@example.com" });

    expect(directory.lookup("room@muc.example.com", "sam")).toBeNull();
    expect(store.urlFor("alice@example.com")).toBeNull();
    expect(store.isKnownAbsent("carol@example.com")).toBe(false);
  });

  test("binding a new client replaces the old one's handlers", () => {
    const directory = new OccupantJidDirectory();
    const binding = createAvatarBinding(new AvatarStore(), directory);
    const first = fakeClient();
    const second = fakeClient();
    binding.bind(first.client);
    binding.bind(second.client);
    first.emitOccupant("room@muc.example.com", "sam", "alice@example.com");
    expect(directory.lookup("room@muc.example.com", "sam")).toBeNull();
    second.emitOccupant("room@muc.example.com", "sam", "bob@example.com");
    expect(directory.lookup("room@muc.example.com", "sam")).toBe("bob@example.com");
  });

  test("evicting an idle entry drops it from the client's known-id cache", async () => {
    const timers: Array<() => void> = [];
    const store = new AvatarStore({
      now: () => 0,
      setTimer: (callback) => { timers.push(callback); return timers.length as unknown as ReturnType<typeof setTimeout>; },
      clearTimer: () => undefined,
    });
    const binding = createAvatarBinding(store, new OccupantJidDirectory());
    const user = fakeClient();
    binding.bind(user.client);
    const release = store.retain("alice@example.com");
    await flush();
    release();
    for (const fire of timers.splice(0)) fire();
    expect(user.forgotten).toEqual(["alice@example.com"]);
  });

  test("a non-online status marks a presence gap that a fresh session turns into unknown holders", () => {
    const directory = new OccupantJidDirectory();
    const binding = createAvatarBinding(new AvatarStore(), directory);
    const statusHooks: Array<(status: { state: "online" | "offline" | "reconnecting" | "error"; detail: string }) => void> = [];
    const user = fakeClient();
    binding.bind({ ...user.client, onStatus: (hook) => { statusHooks.push(hook); return () => undefined; } });
    user.emitOccupant("room@muc.example.com", "sam", "alice@example.com");
    for (const hook of statusHooks) hook({ state: "reconnecting", detail: "" });
    directory.beginFreshSession();
    expect(directory.lookup("room@muc.example.com", "sam")).toBeNull();
  });
});

