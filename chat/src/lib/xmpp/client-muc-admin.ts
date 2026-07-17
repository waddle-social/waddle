/**
 * MUC administration and membership extracted from `BrowserXmppClient`
 * (stage-2 decomposition of `client.ts`): affiliation-based member
 * listing (XEP-0045 §9), affiliation changes, the community-owner
 * probe, and the Waddle admin V1/V2 ad-hoc command wrappers for users,
 * spaces, and channels.
 */
import type { MemberSummary } from "../chat-types";
import { jidLocalpart } from "./jid";
import { stanzaErrorContext } from "./stanza-error-context";
import type { ListRoomMembersOptions, XmppErrorEvent } from "./types";
import type {
  WasmAdminChannelRef,
  WasmAdminChannelType,
  WasmAdminChannelsAffiliationsResult,
  WasmAdminChannelsKickResult,
  WasmAdminChannelsListResult,
  WasmAdminChannelsOccupantsResult,
  WasmAdminChannelsSetAffiliationResult,
  WasmAdminSpaceRef,
  WasmAdminSpacesListResult,
  WasmAdminSpacesMembersResult,
  WasmAdminSpacesSetRoleResult,
  WasmRoomMember,
} from "./wasm-types";

/**
 * Page returned by the V1 admin Users panel back-end. Typed mirror
 * of the `WaddleAdminUsersPage` struct on the wasm side so the chat
 * layer never has to touch raw JSON. `nextCursor` is `null` when
 * the page is the final one.
 */
export interface AdminUserEntry {
  jid: string;
  display_name?: string | null;
  has_owner_hat: boolean;
}
export interface AdminUsersPage {
  entries: AdminUserEntry[];
  next_cursor?: string | null;
}

export class RoomMemberListUnavailableError extends Error {
  constructor(message = "Member list is temporarily unavailable.") {
    super(message);
    this.name = "RoomMemberListUnavailableError";
  }
}


/**
 * Structural subset of the WASM client the MUC admin module drives.
 * The generated bindings type most of these as `Promise<any>`;
 * narrowing here keeps every downstream value typed.
 */
export type MucAdminWasmClient = {
  list_room_members?: (roomJid: string, affiliation: string) => Promise<WasmRoomMember[] | null | undefined>;
  set_room_affiliation?: (roomJid: string, jid: string, affiliation: string) => Promise<unknown>;
  is_community_owner?: () => Promise<boolean>;
  admin_users_list?: (
    prefix: string | null,
    pageSize: number | null,
    afterCursor: string | null,
  ) => Promise<AdminUsersPage>;
  admin_spaces_list?: (args: unknown) => Promise<WasmAdminSpacesListResult>;
  admin_spaces_create?: (args: unknown) => Promise<WasmAdminSpaceRef>;
  admin_spaces_update?: (args: unknown) => Promise<WasmAdminSpaceRef>;
  admin_spaces_delete?: (args: unknown) => Promise<boolean>;
  admin_spaces_members?: (args: unknown) => Promise<WasmAdminSpacesMembersResult>;
  admin_spaces_set_role?: (args: unknown) => Promise<WasmAdminSpacesSetRoleResult>;
  admin_channels_list?: (args: unknown) => Promise<WasmAdminChannelsListResult>;
  admin_channels_create?: (args: unknown) => Promise<WasmAdminChannelRef>;
  admin_channels_update?: (args: unknown) => Promise<WasmAdminChannelRef>;
  admin_channels_delete?: (args: unknown) => Promise<boolean>;
  admin_channels_occupants?: (args: unknown) => Promise<WasmAdminChannelsOccupantsResult>;
  admin_channels_affiliations?: (args: unknown) => Promise<WasmAdminChannelsAffiliationsResult>;
  admin_channels_set_affiliation?: (args: unknown) => Promise<WasmAdminChannelsSetAffiliationResult>;
  admin_channels_kick?: (args: unknown) => Promise<WasmAdminChannelsKickResult>;
};

type MucAdminDeps = {
  requireConnectedXmpp: () => Promise<MucAdminWasmClient>;
  roomJidForChannel: (channelId: string) => string;
  emitError: (event: XmppErrorEvent) => void;
};

export class MucAdmin {
  constructor(private readonly deps: MucAdminDeps) {}

  async listRoomMembers(channelId: string, options?: ListRoomMembersOptions): Promise<MemberSummary[]> {
    const xmpp = await this.deps.requireConnectedXmpp();
    const roomJid = options?.roomJid ?? this.deps.roomJidForChannel(channelId);
    const listMembers = xmpp.list_room_members
      ? async (affiliation: "owner" | "admin" | "member" | "outcast") => await xmpp.list_room_members?.(roomJid, affiliation)
      : null;
    if (!listMembers) {
      this.deps.emitError({
        kind: "member-query",
        recoverable: false,
        reason: "binding-missing",
      });
      throw new Error("missing list_room_members");
    }
    const affiliations = ["owner", "admin", "member", "outcast"] as const; const members: MemberSummary[] = []; const failedAffiliations: string[] = [];
    for (const affiliation of affiliations) {
      try {
        const result = await listMembers(affiliation);
        for (const item of result ?? []) {
          if (!item.jid) continue;
          members.push({
            jid: item.jid,
            username: jidLocalpart(item.jid),
            avatar_url: null,
            affiliation,
            joined_at: "",
          });
        }
      } catch (error) {
        failedAffiliations.push(affiliation);
        this.deps.emitError({
          kind: "member-query",
          recoverable: true,
          reason: options?.roomJid
            ? "affiliation-query-failed"
            : "affiliation-query-failed-reconstructed-room",
          affiliation,
          cause: error,
          ...stanzaErrorContext(error),
        });
      }
    }
    if (members.length === 0 && failedAffiliations.length > 0) {
      throw new RoomMemberListUnavailableError();
    }
    return members;
  }

  async setRoomAffiliation(channelId: string, jid: string, affiliation: MemberSummary["affiliation"]): Promise<void> {
    const xmpp = await this.deps.requireConnectedXmpp();
    await xmpp.set_room_affiliation?.(this.deps.roomJidForChannel(channelId), jid, affiliation === "none" ? "none" : affiliation);
  }

  /**
   * Probe the `urn:waddle:admin:users:list:0` ad-hoc command to
   * decide whether the authenticated user is the community owner. The
   * underlying wasm binding swallows stanza errors and returns `false`
   * — that includes `<forbidden/>` (not an owner) and transient
   * failures (server-side errors). The admin route treats `false` as
   * "show the empty state" in both cases.
   */
  async isCommunityOwner(): Promise<boolean> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.is_community_owner) return false;
    try { return await xmpp.is_community_owner(); } catch { return false; }
  }

  /**
   * Call the admin Users panel back-end and return one page of users.
   * `prefix` is case-insensitive; `pageSize` is clamped server-side to
   * 1..200 (default 50); `afterCursor` is an opaque value returned by
   * a previous call. Rejects on stanza error so the UI can render an
   * explicit failure state.
   */
  async adminUsersList(opts: { prefix?: string | null; pageSize?: number | null; afterCursor?: string | null } = {}): Promise<AdminUsersPage> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_users_list) {
      throw new Error("admin_users_list binding missing — server does not support admin V1");
    }
    return await xmpp.admin_users_list(opts.prefix ?? null, opts.pageSize ?? null, opts.afterCursor ?? null);
  }

  /**
   * Admin V2 spaces: list. Wraps `WaddleClient.admin_spaces_list`.
   *
   * The wasm method consumes a serde-typed `WaddleAdminSpacesListArgs`
   * struct, so this wrapper must pass snake_case keys verbatim.
   */
  async adminSpacesList(opts: { prefix?: string | null; pageSize?: number | null; afterCursor?: string | null } = {}): Promise<WasmAdminSpacesListResult> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_spaces_list) {
      throw new Error("admin_spaces_list binding missing — server does not support admin V2");
    }
    return await xmpp.admin_spaces_list({
      prefix: opts.prefix ?? null,
      page_size: opts.pageSize ?? null,
      after_cursor: opts.afterCursor ?? null,
    });
  }

  async adminSpacesCreate(opts: { name: string; description?: string | null; iconUrl?: string | null }): Promise<WasmAdminSpaceRef> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_spaces_create) throw new Error("admin_spaces_create binding missing");
    return await xmpp.admin_spaces_create({
      name: opts.name,
      description: opts.description ?? null,
      icon_url: opts.iconUrl ?? null,
    });
  }

  async adminSpacesUpdate(opts: { spaceJid: string; spaceNode?: string | null; name?: string | null; description?: string | null; iconUrl?: string | null }): Promise<WasmAdminSpaceRef> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_spaces_update) throw new Error("admin_spaces_update binding missing");
    return await xmpp.admin_spaces_update({
      space_jid: opts.spaceJid,
      space_node: opts.spaceNode ?? null,
      name: opts.name ?? null,
      description: opts.description ?? null,
      icon_url: opts.iconUrl ?? null,
    });
  }

  async adminSpacesDelete(opts: { spaceJid: string; spaceNode?: string | null }): Promise<boolean> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_spaces_delete) throw new Error("admin_spaces_delete binding missing");
    return await xmpp.admin_spaces_delete({
      space_jid: opts.spaceJid,
      space_node: opts.spaceNode ?? null,
      confirm: "yes",
    });
  }

  async adminSpacesMembers(opts: { spaceJid: string; spaceNode?: string | null; pageSize?: number | null; afterCursor?: string | null }): Promise<WasmAdminSpacesMembersResult> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_spaces_members) throw new Error("admin_spaces_members binding missing");
    return await xmpp.admin_spaces_members({
      space_jid: opts.spaceJid,
      space_node: opts.spaceNode ?? null,
      page_size: opts.pageSize ?? null,
      after_cursor: opts.afterCursor ?? null,
    });
  }

  async adminSpacesSetRole(opts: { spaceJid: string; spaceNode?: string | null; memberJid: string; role: "owner" | "admin" | "member" | "none" }): Promise<WasmAdminSpacesSetRoleResult> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_spaces_set_role) throw new Error("admin_spaces_set_role binding missing");
    return await xmpp.admin_spaces_set_role({
      space_jid: opts.spaceJid,
      space_node: opts.spaceNode ?? null,
      member_jid: opts.memberJid,
      role: opts.role,
    });
  }

  async adminChannelsList(opts: { spaceJid?: string | null; spaceNode?: string | null; prefix?: string | null; pageSize?: number | null; afterCursor?: string | null } = {}): Promise<WasmAdminChannelsListResult> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_channels_list) throw new Error("admin_channels_list binding missing");
    return await xmpp.admin_channels_list({
      space_jid: opts.spaceJid ?? null,
      space_node: opts.spaceNode ?? null,
      prefix: opts.prefix ?? null,
      page_size: opts.pageSize ?? null,
      after_cursor: opts.afterCursor ?? null,
    });
  }

  async adminChannelsCreate(opts: { name: string; topic?: string | null; channelType?: WasmAdminChannelType | null; spaceJid?: string | null; spaceNode?: string | null; isPublic?: boolean | null; membersOnly?: boolean | null }): Promise<WasmAdminChannelRef> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_channels_create) throw new Error("admin_channels_create binding missing");
    return await xmpp.admin_channels_create({
      name: opts.name,
      topic: opts.topic ?? null,
      channel_type: opts.channelType ?? null,
      space_jid: opts.spaceJid ?? null,
      space_node: opts.spaceNode ?? null,
      is_public: opts.isPublic ?? null,
      members_only: opts.membersOnly ?? null,
    });
  }

  async adminChannelsUpdate(opts: { channelJid: string; name?: string | null; topic?: string | null; channelType?: WasmAdminChannelType | null; isPublic?: boolean | null; membersOnly?: boolean | null }): Promise<WasmAdminChannelRef> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_channels_update) throw new Error("admin_channels_update binding missing");
    return await xmpp.admin_channels_update({
      channel_jid: opts.channelJid,
      name: opts.name ?? null,
      topic: opts.topic ?? null,
      channel_type: opts.channelType ?? null,
      is_public: opts.isPublic ?? null,
      members_only: opts.membersOnly ?? null,
    });
  }

  async adminChannelsDelete(opts: { channelJid: string }): Promise<boolean> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_channels_delete) throw new Error("admin_channels_delete binding missing");
    return await xmpp.admin_channels_delete({ channel_jid: opts.channelJid, confirm: "yes" });
  }

  async adminChannelsOccupants(opts: { channelJid: string; pageSize?: number | null; afterCursor?: string | null }): Promise<WasmAdminChannelsOccupantsResult> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_channels_occupants) throw new Error("admin_channels_occupants binding missing");
    return await xmpp.admin_channels_occupants({
      channel_jid: opts.channelJid,
      page_size: opts.pageSize ?? null,
      after_cursor: opts.afterCursor ?? null,
    });
  }

  async adminChannelsAffiliations(opts: { channelJid: string; filter?: string | null; pageSize?: number | null; afterCursor?: string | null }): Promise<WasmAdminChannelsAffiliationsResult> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_channels_affiliations) throw new Error("admin_channels_affiliations binding missing");
    return await xmpp.admin_channels_affiliations({
      channel_jid: opts.channelJid,
      filter: opts.filter ?? null,
      page_size: opts.pageSize ?? null,
      after_cursor: opts.afterCursor ?? null,
    });
  }

  async adminChannelsSetAffiliation(opts: { channelJid: string; memberJid: string; affiliation: "owner" | "admin" | "member" | "none" | "outcast"; reason?: string | null }): Promise<WasmAdminChannelsSetAffiliationResult> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_channels_set_affiliation) throw new Error("admin_channels_set_affiliation binding missing");
    return await xmpp.admin_channels_set_affiliation({
      channel_jid: opts.channelJid,
      member_jid: opts.memberJid,
      affiliation: opts.affiliation,
      reason: opts.reason ?? null,
    });
  }

  async adminChannelsKick(opts: { channelJid: string; occupantJid: string; reason?: string | null }): Promise<WasmAdminChannelsKickResult> {
    const xmpp = await this.deps.requireConnectedXmpp();
    if (!xmpp.admin_channels_kick) throw new Error("admin_channels_kick binding missing");
    return await xmpp.admin_channels_kick({
      channel_jid: opts.channelJid,
      occupant_jid: opts.occupantJid,
      reason: opts.reason ?? null,
    });
  }
}
