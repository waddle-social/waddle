import { ref, computed, watch, type Ref } from "vue";
import type { SpaceSummary, ChannelSummary, GroupDmSummary, MemberSummary } from "@/lib/chat-types";
import type { WaddleSession } from "@/lib/server-auth";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { CommunityFormData, CreateFormData, ChannelEditFormData, CreateChannelResult } from "@/lib/chat-ui";
import { defaultCreateForm, defaultCreateFormForContext } from "@/lib/chat-ui";
import {
  configureMucRoom,
  createMucRoom,
  createSpaceNode,
  createMucInSpace,
  createSpaceWithMuc,
  moveMucToSpace,
} from "@/lib/xmpp/protocol-helpers";
import { mucServiceDomain, spacesServiceDomain } from "@/lib/xmpp/discovery";
import { barePeerJid, jidLocalpart } from "@/lib/xmpp/jid";

interface LoadSpaceOptions {
  loadStructure?: boolean;
}

interface LoadStructureOptions {
  /** When true, skip auto-selection of any channel and set activeChannelId to null. */
  noChannelSelect?: boolean;
}

export type MemberLoadState = "idle" | "loading" | "ready" | "unavailable";

export function useWaddleDirectory(
  xmppClient: Ref<BrowserXmppClient | null>,
  session: Ref<WaddleSession | null>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
) {
  const waddles = ref<SpaceSummary[]>([]);
  const channels = ref<ChannelSummary[]>([]);
  const groupDms = ref<GroupDmSummary[]>([]);
  const members = ref<MemberSummary[]>([]);
  const memberLoadState = ref<MemberLoadState>("idle");
  const serverRole = ref<SpaceSummary["role"]>(null);
  const mucServiceJid = ref<string | null>(null);
  const spacesServiceJid = ref<string | null>(null);

  const activeChannelId: Ref<string | null> = ref(null);
  const selectedSpaceId: Ref<string | null> = ref(null);

  const isLoadingStructure = ref(false);
  const hasLoadedStructure = ref(false);
  const isSubmitting = ref(false);

  let spaceRequestId = 0;
  let structureRequestId = 0;
  let memberRequestId = 0;
  const memberSnapshotsByChannelId = new Map<string, MemberSummary[]>();

  const editWaddleForm = ref<CommunityFormData>({
    name: "",
    description: "",
    is_public: true,
  });

  const createChannelForm = ref<CreateFormData>(defaultCreateForm());

  const editChannelForm = ref<ChannelEditFormData>({
    name: "",
    description: "",
    position: 0,
  });

  const currentChannel = computed(
    () => channels.value.find((c) => c.id === activeChannelId.value) ?? null,
  );
  const activeSpaceId = computed(() => currentChannel.value?.spaceId ?? selectedSpaceId.value);
  const currentSpace = computed(() =>
    activeSpaceId.value
      ? waddles.value.find((w) => w.id === activeSpaceId.value) ?? null
      : null,
  );
  const isEmptyDeployment = computed(() =>
    waddles.value.length === 0 && channels.value.length === 0 && !isLoadingStructure.value,
  );

  const currentRole = computed(() => {
    if (!session.value || !currentSpace.value) return null;
    return (
      members.value.find((m) => m.jid === barePeerJid(session.value!.jid))?.affiliation ??
      currentSpace.value.role ??
      null
    );
  });
  const currentSpaceRole = computed(() => currentSpace.value?.role ?? null);

  const sortedSpaces = computed(() =>
    [...waddles.value].sort((a, b) =>
      a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
    ),
  );

  const sortedChannels = computed(() =>
    [...channels.value]
      .filter((channel) => !channel.isGroupDm)
      .sort(
      (a, b) =>
        (a.position ?? 0) - (b.position ?? 0) ||
        a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
    ),
  );

  const sortedMembers = computed(() => {
    const order = { owner: 0, admin: 1, member: 2, outcast: 3, none: 4 } as const;
    return [...members.value].sort(
      (a, b) =>
        (order[a.affiliation] ?? 4) - (order[b.affiliation] ?? 4) ||
        a.username.localeCompare(b.username, undefined, { sensitivity: "base" }),
    );
  });

  const canManageCommunity = computed(() =>
    ["owner"].includes(serverRole.value ?? "") ||
    ["owner"].includes(currentSpaceRole.value ?? currentRole.value ?? ""),
  );

  const canManageChannels = computed(() =>
    ["owner"].includes(serverRole.value ?? "") ||
    ["owner"].includes(currentSpaceRole.value ?? currentRole.value ?? ""),
  );

  const canManageMembers = computed(() =>
    ["owner", "admin"].includes(currentRole.value ?? ""),
  );

  watch(currentSpace, (w) => {
    if (w) {
      editWaddleForm.value = {
        name: w.name,
        description: w.description ?? "",
        is_public: w.is_public ?? true,
      };
    }
  });

  watch(currentChannel, (c) => {
    if (c) {
      selectedSpaceId.value = c.spaceId ?? null;
      editChannelForm.value = {
        name: c.name,
        description: c.description ?? "",
        position: c.position ?? 0,
        pinPermission: c.pinPermission,
      };
    }
  });

  function resetForms() {
    createChannelForm.value = defaultCreateForm();
  }

  function beginMemberLoad(channelId: string) {
    memberLoadState.value = "loading";
    members.value = memberSnapshotsByChannelId.get(channelId) ?? [];
  }

  function completeMemberLoad(channelId: string, freshMembers: MemberSummary[]) {
    memberSnapshotsByChannelId.set(channelId, freshMembers);
    members.value = freshMembers;
    memberLoadState.value = "ready";
  }

  function failMemberLoad(channelId: string, error: unknown) {
    console.warn("Unable to load room members", error);
    members.value = memberSnapshotsByChannelId.get(channelId) ?? members.value;
    memberLoadState.value = "unavailable";
  }

  function prepareCreateChannelForContext(spaceId: string | null = currentSpace.value?.id ?? null) {
    createChannelForm.value = defaultCreateFormForContext(spaceId);
  }

  async function loadStructure(preferredChannelId?: string | null, opts?: LoadStructureOptions): Promise<string | null> {
    if (!xmppClient.value) return null;

    const requestId = ++structureRequestId;
    isLoadingStructure.value = true;
    clearActionError();

    try {
      const topology = await xmppClient.value.discoverTopology();

      if (requestId !== structureRequestId) {
        return null;
      }

      serverRole.value = topology.serverRole ?? null;
      mucServiceJid.value = topology.services?.muc ?? null;
      spacesServiceJid.value = topology.services?.spaces ?? null;
      const discoveredSpaces = topology.spaces.map((space) => ({
        id: space.id,
        name: space.name,
        role: space.role ?? null,
      }) satisfies SpaceSummary);
      waddles.value = discoveredSpaces;

      const allRooms: ChannelSummary[] = topology.rooms.map((c) => ({
        id: c.id,
        name: c.name,
        jid: c.jid,
        spaceId: c.spaceId,
        channel_type: c.channelType,
        position: c.position,
        features: c.features,
        isGroupDm: c.isGroupDm,
        // #422: hydrated from the room's disco-info extended form so
        // non-owners get correct Pin action visibility without owner
        // affiliation.
        ...(c.pinPermission ? { pinPermission: c.pinPermission } : {}),
      }));

      const channelList = allRooms.filter((channel) => !channel.isGroupDm);
      groupDms.value = allRooms
        .filter((channel): channel is ChannelSummary & { jid: string } => !!channel.isGroupDm && !!channel.jid)
        .map((channel) => ({
          id: channel.id,
          roomJid: channel.jid,
          name: channel.name,
          position: channel.position,
          features: channel.features,
          pinPermission: channel.pinPermission,
          autojoin: topology.rooms.find((room) => room.jid === channel.jid)?.autojoin,
        }));
      channels.value = allRooms;
      hasLoadedStructure.value = true;

      if (opts?.noChannelSelect) {
        activeChannelId.value = null;
        selectedSpaceId.value = null;
        members.value = [];
        memberLoadState.value = "idle";
        resetForms();
        return null;
      }

      const nextChannelId =
        preferredChannelId && channelList.some((c) => c.id === preferredChannelId)
          ? preferredChannelId
          : activeChannelId.value && channelList.some((c) => c.id === activeChannelId.value)
            ? activeChannelId.value
            : channelList.find((c) => c.id === "chat" || c.name.toLowerCase() === "chat")?.id ?? channelList[0]?.id ?? null;

      activeChannelId.value = nextChannelId;
      const nextChannel = channelList.find((channel) => channel.id === nextChannelId);
      selectedSpaceId.value = nextChannel ? nextChannel.spaceId ?? null : selectedSpaceId.value;
      if (nextChannelId && xmppClient.value) {
        const memberReqId = ++memberRequestId;
        beginMemberLoad(nextChannelId);
        try {
          const freshMembers = await xmppClient.value.listRoomMembers(nextChannelId, { roomJid: nextChannel?.jid });
          if (requestId === structureRequestId && memberReqId === memberRequestId) {
            completeMemberLoad(nextChannelId, freshMembers);
          }
        } catch (e) {
          if (requestId === structureRequestId && memberReqId === memberRequestId) {
            failMemberLoad(nextChannelId, e);
          }
        }
      } else {
        members.value = [];
        memberLoadState.value = "idle";
      }
      resetForms();
      return nextChannelId;
    } catch (e) {
      if (requestId === structureRequestId) {
        actionError.value = normalizeError(e);
      }
      return null;
    } finally {
      if (requestId === structureRequestId) {
        isLoadingStructure.value = false;
      }
    }
  }

  async function loadSpace(
    options: LoadSpaceOptions = {},
  ) {
    const requestId = ++spaceRequestId;
    clearActionError();

    if (requestId !== spaceRequestId) return null;

    if (options.loadStructure !== false) {
      return loadStructure();
    } else {
      return null;
    }
  }

  async function reloadChannelMembers(channelId: string): Promise<void> {
    if (!xmppClient.value) return;

    const channel = channels.value.find((c) => c.id === channelId);
    const requestId = ++memberRequestId;
    beginMemberLoad(channelId);

    try {
      const freshMembers = await xmppClient.value.listRoomMembers(channelId, { roomJid: channel?.jid });
      if (requestId !== memberRequestId) return;
      completeMemberLoad(channelId, freshMembers);
    } catch (e) {
      if (requestId === memberRequestId) {
        failMemberLoad(channelId, e);
      }
    }
  }

  async function updateWaddle() {
    actionError.value = "Space settings are managed by the server configuration.";
  }

  async function deleteWaddle() {
    actionError.value = "Deleting the configured space is not available in the XMPP-only client.";
  }

  async function createChannel(): Promise<CreateChannelResult | undefined> {
    const form = createChannelForm.value;
    if (!xmppClient.value || !session.value) return undefined;

    const xmppAgent = xmppClient.value.agent;
    if (!xmppAgent) {
      actionError.value = "XMPP connection not available";
      return undefined;
    }

    const mucService = mucServiceJid.value ?? mucServiceDomain(session.value.jid);
    const spacesService = spacesServiceJid.value ?? spacesServiceDomain(session.value.jid);
    const nick = session.value.username;

    isSubmitting.value = true;
    clearActionError();

    try {
      if (form.intent === "muc") {
        if (!form.name.trim()) return undefined;

        const { roomJid } = await createMucRoom(xmppAgent, mucService, {
          roomLocalpart: form.name.trim().toLowerCase().replace(/\s+/g, "-"),
          nick,
          name: form.name.trim(),
          description: form.description.trim() || undefined,
          mucType: form.muc_type,
        });

        const channelId = jidLocalpart(roomJid);
        createChannelForm.value = defaultCreateForm();
        await loadStructure(channelId);
        return {
          intent: "muc",
          channelId,
          channelJid: roomJid,
          channelName: form.name.trim(),
          channelType: form.muc_type,
        } satisfies CreateChannelResult;
      }

      if (form.intent === "space") {
        if (!form.name.trim()) return undefined;

        const { node } = await createSpaceNode(xmppAgent, spacesService, {
          name: form.name.trim(),
          description: form.description.trim() || undefined,
        });

        createChannelForm.value = defaultCreateForm();
        await loadStructure(null, { noChannelSelect: true });
        selectedSpaceId.value = node;
        return {
          intent: "space",
          spaceId: node,
          spaceName: form.name.trim(),
        } satisfies CreateChannelResult;
      }

      if (form.intent === "space-muc") {
        if (!form.name.trim() || !form.space_node.trim()) return undefined;

        const spaceNode = form.space_node;
        const { roomJid } = await createMucInSpace(xmppAgent, mucService, spacesService, {
          roomLocalpart: form.name.trim().toLowerCase().replace(/\s+/g, "-"),
          nick,
          name: form.name.trim(),
          description: form.description.trim() || undefined,
          mucType: form.muc_type,
          spaceNode,
        });

        const channelId = jidLocalpart(roomJid);
        createChannelForm.value = defaultCreateForm();
        await loadStructure(channelId);
        return {
          intent: "space-muc",
          channelId,
          channelJid: roomJid,
          channelName: form.name.trim(),
          channelType: form.muc_type,
          spaceNode,
        } satisfies CreateChannelResult;
      }

      if (form.intent === "space-with-muc") {
        if (!form.space_name.trim() || !form.muc_name.trim()) return undefined;

        const { roomJid, spaceNode } = await createSpaceWithMuc(
          xmppAgent,
          mucService,
          spacesService,
          {
            spaceName: form.space_name.trim(),
            spaceDescription: form.space_description.trim() || undefined,
            roomLocalpart: form.muc_name.trim().toLowerCase().replace(/\s+/g, "-"),
            nick,
            mucName: form.muc_name.trim(),
            mucDescription: form.muc_description.trim() || undefined,
            mucType: form.muc_type,
          },
        );

        const channelId = jidLocalpart(roomJid);
        createChannelForm.value = defaultCreateForm();
        await loadStructure(channelId);
        return {
          intent: "space-with-muc",
          channelId,
          channelJid: roomJid,
          channelName: form.muc_name.trim(),
          channelType: form.muc_type,
          spaceNode,
        } satisfies CreateChannelResult;
      }

      return undefined;
    } catch (e) {
      actionError.value = normalizeError(e);
      return undefined;
    } finally {
      isSubmitting.value = false;
    }
  }

  async function updateChannel(): Promise<boolean> {
    const channel = currentChannel.value;
    if (!channel?.jid) {
      actionError.value = "Cannot edit a channel without a known room JID.";
      return false;
    }
    const client = xmppClient.value;
    if (!client) {
      actionError.value = "XMPP session is not ready.";
      return false;
    }
    isSubmitting.value = true;
    try {
      await configureMucRoom(client, channel.jid, {
        name: editChannelForm.value.name.trim(),
        description: editChannelForm.value.description.trim(),
        pinPermission: editChannelForm.value.pinPermission,
      });
      // Optimistically apply locally so the action-sheet visibility
      // updates without waiting for a re-fetch. The server is
      // authoritative — a subsequent owner-config GET would refresh.
      const idx = channels.value.findIndex((c) => c.id === channel.id);
      if (idx >= 0) {
        channels.value[idx] = {
          ...channels.value[idx],
          name: editChannelForm.value.name.trim(),
          description: editChannelForm.value.description.trim(),
          // Only stamp pinPermission locally when the user actually
          // chose a value in the dialog; otherwise the server-side
          // value (preserved by GET-then-SET) remains authoritative.
          ...(editChannelForm.value.pinPermission !== undefined
            ? { pinPermission: editChannelForm.value.pinPermission }
            : {}),
        };
      }
      return true;
    } catch (error) {
      actionError.value = normalizeError(error);
      return false;
    } finally {
      isSubmitting.value = false;
    }
  }

  async function deleteChannel() {
    actionError.value = "Channel deletion is not available until the XMPP command exists.";
  }

  /**
   * Move a channel to a different XEP-0503 Space. Returns `true` on
   * success.
   *
   * Implementation publishes the channel's `<conference>` bookmark
   * to the target Space; the server (per XEP-0503 single-membership
   * enforcement) retracts the item from every other Space before
   * persisting, so no separate retract is required here.
   *
   * Re-loads the channel structure after a successful move so the
   * sidebar groups reflect the new membership.
   */
  async function moveChannelToSpace(
    channelId: string,
    targetSpaceId: string,
  ): Promise<boolean> {
    const channel = channels.value.find((c) => c.id === channelId);
    if (!channel?.jid) {
      actionError.value = "Cannot move a channel without a known room JID.";
      return false;
    }
    if (channel.spaceId === targetSpaceId) {
      // No-op: already in the target Space. Surface as success so
      // the UI clears its pending state.
      return true;
    }
    const client = xmppClient.value;
    const sess = session.value;
    if (!client?.agent || !sess) {
      actionError.value = "XMPP session is not ready.";
      return false;
    }
    const spacesService = spacesServiceJid.value ?? spacesServiceDomain(sess.jid);
    isSubmitting.value = true;
    try {
      await moveMucToSpace(client.agent, spacesService, targetSpaceId, channel.jid, {
        name: channel.name,
        autojoin: true,
      });
      // Optimistically update local state so the channel re-groups
      // in the sidebar before the next structure reload.
      const idx = channels.value.findIndex((c) => c.id === channelId);
      if (idx >= 0) {
        channels.value[idx] = { ...channels.value[idx], spaceId: targetSpaceId };
      }
      await loadStructure(channelId);
      return true;
    } catch (error) {
      actionError.value = normalizeError(error);
      return false;
    } finally {
      isSubmitting.value = false;
    }
  }

  function clearData() {
    spaceRequestId++;
    structureRequestId++;
    memberRequestId++;
    waddles.value = [];
    channels.value = [];
    groupDms.value = [];
    members.value = [];
    memberLoadState.value = "idle";
    memberSnapshotsByChannelId.clear();
    serverRole.value = null;
    mucServiceJid.value = null;
    spacesServiceJid.value = null;
    activeChannelId.value = null;
    selectedSpaceId.value = null;
    hasLoadedStructure.value = false;
  }

  return {
    waddles,
    channels,
    groupDms,
    members,
    memberLoadState,
    serverRole,
    mucServiceJid,
    activeSpaceId,
    activeChannelId,
    isLoadingStructure,
    hasLoadedStructure,
    isSubmitting,
    editWaddleForm,
    createChannelForm,
    editChannelForm,
    currentSpace,
    isEmptyDeployment,
    currentChannel,
    currentRole,
    sortedSpaces,
    sortedChannels,
    sortedMembers,
    canManageCommunity,
    canManageChannels,
    canManageMembers,
    loadSpace,
    loadStructure,
    reloadChannelMembers,
    prepareCreateChannelForContext,
    updateWaddle,
    deleteWaddle,
    createChannel,
    updateChannel,
    deleteChannel,
    moveChannelToSpace,
    clearData,
    resetForms,
  };
}
