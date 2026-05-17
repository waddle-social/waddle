// Tests for `UserProfileDrawer.vue` — the chat surface that opens when
// you click someone's avatar. The drawer needs to fetch BOTH the
// XEP-0163 PEP profile (mood/activity/tune) and the XEP-0292 vCard4
// (FN/pronouns/note/url) in parallel and render whichever fields are
// present.
//
// There's no @vue/test-utils harness in this repo, so we mirror the
// vcard4-editor test pattern: a small in-memory model that captures
// the same fetch + render decisions the component makes, exercised
// against stubbed `BrowserXmppClient` reads.

import { describe, expect, mock, test } from "bun:test";
import type { UserPepProfile } from "../src/lib/xmpp/pep-types";
import type { VCard4Profile } from "../src/lib/xmpp/vcard4-types";

interface DrawerClient {
  fetchUserPepProfile: (jid: string) => Promise<UserPepProfile>;
  fetchVCard4: (jid: string) => Promise<VCard4Profile | null>;
}

interface DrawerState {
  pepProfile: UserPepProfile | null;
  vcard: VCard4Profile | null;
  loading: boolean;
}

interface DrawerView {
  fullNameToShow: string | null;
  pronouns: string | null;
  bio: string | null;
  website: string | null;
  hasVCardSection: boolean;
  hasMood: boolean;
  hasActivity: boolean;
  hasTune: boolean;
}

/**
 * Mirrors `UserProfileDrawer.vue`'s drawer-open flow: fetch PEP +
 * vCard in parallel via `Promise.allSettled`, fall back to `null` on
 * either side independently so a transient failure on one fetch
 * doesn't blank the other.
 */
function createDrawer(client: DrawerClient, username: string) {
  const state: DrawerState = {
    pepProfile: null,
    vcard: null,
    loading: false,
  };

  async function open(jid: string) {
    state.loading = true;
    const [pep, vcard] = await Promise.allSettled([
      client.fetchUserPepProfile(jid),
      client.fetchVCard4(jid),
    ]);
    state.pepProfile = pep.status === "fulfilled" ? pep.value : null;
    state.vcard = vcard.status === "fulfilled" ? vcard.value : null;
    state.loading = false;
  }

  function close() {
    state.pepProfile = null;
    state.vcard = null;
  }

  /**
   * Encodes the same conditional-render decisions the template makes.
   * Asserting against this lets the tests pin down the user-visible
   * output without booting Vue.
   */
  function view(): DrawerView {
    const fn = state.vcard?.fullName?.trim() ?? "";
    const fullNameToShow = fn && fn !== username ? fn : null;
    const pronouns = state.vcard?.pronouns?.trim() ?? null;
    const bio = state.vcard?.note?.trim() ?? null;
    let website: string | null = null;
    const rawUrl = state.vcard?.url?.trim();
    if (rawUrl) {
      try {
        const parsed = new URL(rawUrl);
        if (parsed.protocol === "http:" || parsed.protocol === "https:") {
          website = parsed.toString();
        }
      } catch {
        website = null;
      }
    }
    return {
      fullNameToShow,
      pronouns: pronouns && pronouns.length > 0 ? pronouns : null,
      bio: bio && bio.length > 0 ? bio : null,
      website,
      hasVCardSection: !!(fullNameToShow || pronouns || bio || website),
      hasMood: !!state.pepProfile?.mood,
      hasActivity: !!state.pepProfile?.activity,
      hasTune: !!state.pepProfile?.tune,
    };
  }

  return { state, open, close, view };
}

const emptyPep: UserPepProfile = { mood: null, activity: null, tune: null };

describe("UserProfileDrawer fetch flow", () => {
  test("opens both fetches in parallel and renders vCard fields", async () => {
    const pepDeferred: { resolve: (value: UserPepProfile) => void } = {
      resolve: () => {},
    };
    const vcardDeferred: { resolve: (value: VCard4Profile | null) => void } = {
      resolve: () => {},
    };
    const pepPromise = new Promise<UserPepProfile>((resolve) => {
      pepDeferred.resolve = resolve;
    });
    const vcardPromise = new Promise<VCard4Profile | null>((resolve) => {
      vcardDeferred.resolve = resolve;
    });
    const fetchUserPepProfile = mock(async () => pepPromise);
    const fetchVCard4 = mock(async () => vcardPromise);
    const drawer = createDrawer(
      { fetchUserPepProfile, fetchVCard4 },
      "romeo",
    );

    const openPromise = drawer.open("romeo@example.com");
    // Both fetches must have been kicked off before either resolves —
    // proves the parallel `Promise.allSettled` shape.
    expect(fetchUserPepProfile).toHaveBeenCalledTimes(1);
    expect(fetchVCard4).toHaveBeenCalledTimes(1);

    vcardDeferred.resolve({
      fullName: "Romeo Montague",
      pronouns: "he/him",
      note: "Star-crossed",
      url: "https://romeo.example.com",
    });
    pepDeferred.resolve({
      mood: { kind: "happy", text: "in love" },
      activity: null,
      tune: null,
    });
    await openPromise;

    const view = drawer.view();
    expect(view.fullNameToShow).toBe("Romeo Montague");
    expect(view.pronouns).toBe("he/him");
    expect(view.bio).toBe("Star-crossed");
    expect(view.website).toBe("https://romeo.example.com/");
    expect(view.hasVCardSection).toBe(true);
    expect(view.hasMood).toBe(true);
  });

  test("hides Name section when vCard FN equals username header", async () => {
    const fetchUserPepProfile = mock(async () => emptyPep);
    const fetchVCard4 = mock(async () => ({ fullName: "alice" } as VCard4Profile));
    const drawer = createDrawer(
      { fetchUserPepProfile, fetchVCard4 },
      "alice",
    );

    await drawer.open("alice@example.com");

    expect(drawer.view().fullNameToShow).toBeNull();
  });

  test("renders nothing when vCard fetch returns null", async () => {
    const fetchUserPepProfile = mock(async () => emptyPep);
    const fetchVCard4 = mock(async () => null);
    const drawer = createDrawer(
      { fetchUserPepProfile, fetchVCard4 },
      "ghost",
    );

    await drawer.open("ghost@example.com");

    const view = drawer.view();
    expect(view.hasVCardSection).toBe(false);
    expect(view.fullNameToShow).toBeNull();
    expect(view.pronouns).toBeNull();
    expect(view.bio).toBeNull();
    expect(view.website).toBeNull();
    expect(drawer.state.vcard).toBeNull();
  });

  test("surfaces PEP profile even if vCard fetch rejects", async () => {
    const fetchUserPepProfile = mock(async () => ({
      mood: { kind: "happy" },
      activity: null,
      tune: null,
    } as UserPepProfile));
    const fetchVCard4 = mock(async () => {
      throw new Error("vcard fetch failed");
    });
    const drawer = createDrawer(
      { fetchUserPepProfile, fetchVCard4 },
      "alice",
    );

    await drawer.open("alice@example.com");

    expect(drawer.state.vcard).toBeNull();
    expect(drawer.state.pepProfile?.mood?.kind).toBe("happy");
    expect(drawer.view().hasMood).toBe(true);
    expect(drawer.view().hasVCardSection).toBe(false);
  });

  test("surfaces vCard even if PEP fetch rejects", async () => {
    const fetchUserPepProfile = mock(async () => {
      throw new Error("pep fetch failed");
    });
    const fetchVCard4 = mock(async () => ({ fullName: "Juliet" } as VCard4Profile));
    const drawer = createDrawer(
      { fetchUserPepProfile, fetchVCard4 },
      "juliet-c",
    );

    await drawer.open("juliet@example.com");

    expect(drawer.state.pepProfile).toBeNull();
    expect(drawer.view().fullNameToShow).toBe("Juliet");
    expect(drawer.view().hasVCardSection).toBe(true);
  });

  test("rejects javascript: URLs from rendering as a website link", async () => {
    const fetchUserPepProfile = mock(async () => emptyPep);
    const fetchVCard4 = mock(async () => ({
      url: "javascript:alert(1)", // eslint-disable-line no-script-url -- testing sanitiser
    } as VCard4Profile));
    const drawer = createDrawer(
      { fetchUserPepProfile, fetchVCard4 },
      "alice",
    );

    await drawer.open("alice@example.com");

    expect(drawer.view().website).toBeNull();
    expect(drawer.view().hasVCardSection).toBe(false);
  });

  test("close clears both PEP and vCard state", async () => {
    const fetchUserPepProfile = mock(async () => emptyPep);
    const fetchVCard4 = mock(async () => ({ fullName: "Romeo" } as VCard4Profile));
    const drawer = createDrawer(
      { fetchUserPepProfile, fetchVCard4 },
      "romeo-c",
    );

    await drawer.open("romeo@example.com");
    expect(drawer.state.vcard).not.toBeNull();
    drawer.close();
    expect(drawer.state.vcard).toBeNull();
    expect(drawer.state.pepProfile).toBeNull();
  });
});
