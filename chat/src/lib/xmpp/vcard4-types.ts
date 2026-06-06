/**
 * Chat-side typed mirror of the XEP-0292 vCard4 payload exchanged with the wasm
 * client over the `fetch_vcard4` / `publish_vcard4` JSI boundary. The wasm side
 * carries a `WaddleVCard4` struct (see `chat/src/lib/xmpp/wasm-types.ts`) and
 * the typed Rust `VCard4` in `waddle-xmpp-client::xep::xep0292`.
 *
 * Every property is optional. Empty strings are treated as "unset" by the
 * editor and translate to `undefined` on the wire — empty `<fn><text/></fn>`
 * round-trips would otherwise persist whitespace and shadow a real value.
 *
 * `pronouns` corresponds to the XEP-0292 `PRONOUNS` property. The wasm boundary
 * always carries the field, but older servers may drop it on publish until the
 * server-side support lands.
 */
export interface VCard4Profile {
  fullName?: string;
  nickname?: string;
  pronouns?: string;
  note?: string;
  url?: string;
  /** Preserved vCard4 PHOTO URI, usually mirrored from XEP-0084 avatar publishing. */
  photoUri?: string;
}

/** Mutable draft used by the editor while the user is typing. */
export interface VCard4Draft {
  fullName: string;
  nickname: string;
  pronouns: string;
  note: string;
  url: string;
  photoUri?: string;
}

/** Returns a fresh draft seeded from `profile`, defaulting absent fields to "". */
export function draftFromProfile(profile: VCard4Profile | null): VCard4Draft {
  const draft: VCard4Draft = {
    fullName: profile?.fullName ?? "",
    nickname: profile?.nickname ?? "",
    pronouns: profile?.pronouns ?? "",
    note: profile?.note ?? "",
    url: profile?.url ?? "",
  };
  if (profile?.photoUri) draft.photoUri = profile.photoUri;
  return draft;
}

/** Snapshots a draft as a profile, trimming whitespace and dropping empties. */
export function profileFromDraft(draft: VCard4Draft): VCard4Profile {
  const profile: VCard4Profile = {};
  const apply = (key: keyof VCard4Profile, value: string) => {
    const trimmed = value.trim();
    if (trimmed) profile[key] = trimmed;
  };
  apply("fullName", draft.fullName);
  apply("nickname", draft.nickname);
  apply("pronouns", draft.pronouns);
  apply("note", draft.note);
  apply("url", draft.url);
  if (draft.photoUri) profile.photoUri = draft.photoUri;
  return profile;
}

/** Returns `true` when the two profiles carry identical values across every field. */
export function profilesEqual(a: VCard4Profile, b: VCard4Profile): boolean {
  return (
    (a.fullName ?? "") === (b.fullName ?? "") &&
    (a.nickname ?? "") === (b.nickname ?? "") &&
    (a.pronouns ?? "") === (b.pronouns ?? "") &&
    (a.note ?? "") === (b.note ?? "") &&
    (a.url ?? "") === (b.url ?? "") &&
    (a.photoUri ?? "") === (b.photoUri ?? "")
  );
}
