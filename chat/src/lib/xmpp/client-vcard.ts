/**
 * vCard/profile surface extracted from `BrowserXmppClient` (stage-2
 * decomposition of `client.ts`): the XEP-0292 vCard4 profile
 * (fetch/publish) and XEP-0084/XEP-0153 user avatar resolution.
 */
import { barePeerJid } from "./jid";
import type { VCard4Profile } from "./vcard4-types";
import { avatarDataUrl } from "./wasm-message-codecs";
import type { WasmAvatar, WasmVCard4 } from "./wasm-types";

/** Structural subset of the WASM client the vCard module drives. */
export type VCardWasmClient = {
  fetch_vcard4?: (jid: string) => Promise<WasmVCard4 | null>;
  publish_vcard4?: (vcard: WasmVCard4) => Promise<unknown>;
  request_avatar?: (jid: string) => Promise<WasmAvatar | null>;
};

type VCardManagerDeps = {
  requireConnectedXmpp: () => Promise<VCardWasmClient>;
};

export class VCardManager {
  constructor(private readonly deps: VCardManagerDeps) {}

  async fetchVCard4(jid: string): Promise<VCard4Profile | null> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const payload = await xmpp.fetch_vcard4?.(jid);
    if (!payload) return null;
    const profile: VCard4Profile = {};
    if (payload.fn) profile.fullName = payload.fn;
    if (payload.nickname) profile.nickname = payload.nickname;
    if (payload.pronouns) profile.pronouns = payload.pronouns;
    if (payload.note) profile.note = payload.note;
    if (payload.url) profile.url = payload.url;
    if (payload.photo_uri) profile.photoUri = payload.photo_uri;
    return profile;
  }

  async publishVCard4(profile: VCard4Profile): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const payload: WasmVCard4 = {};
    if (profile.fullName) payload.fn = profile.fullName;
    if (profile.nickname) payload.nickname = profile.nickname;
    if (profile.pronouns) payload.pronouns = profile.pronouns;
    if (profile.note) payload.note = profile.note;
    if (profile.url) payload.url = profile.url;
    if (profile.photoUri) payload.photo_uri = profile.photoUri;
    await xmpp.publish_vcard4?.(payload);
  }

  async fetchUserAvatar(jid: string): Promise<string | null> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const bareJid = barePeerJid(jid);
    if (xmpp.request_avatar) {
      const avatar = await xmpp.request_avatar(bareJid);
      if (avatar?.url) return avatar.url;
      if (avatar?.data) return avatarDataUrl(avatar.data, avatar.mime_type);
    }
    return null;
  }
}
