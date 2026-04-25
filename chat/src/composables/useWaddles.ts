import { ref, computed, watch, type Ref } from "vue";
import type { SpaceSummary, ChannelSummary, MemberSummary } from "@/lib/chat-types";
import type { WaddleSession } from "@/lib/server-auth";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { CommunityFormData, CreateFormData, ChannelEditFormData, CreateChannelResult } from "@/lib/chat-ui";
import { defaultCreateForm } from "@/lib/chat-ui";
import {
  createMucRoom,
  createSpaceNode,
  createMucInSpace,
  createSpaceWithMuc,
} from "@/lib/xmpp/protocol-helpers";
import { jidDomain } from "@/lib/xmpp/jid";

interface LoadSpaceOptions {
  loadStructure?: boolean;
}

interface LoadStructureOptions {
  /** When true, skip auto-selection of any channel and set activeChannelId to null. */
  noChannelSelect?: boolean;
}

export function useWaddles(
  _api: Ref<null>,
  xmppClient: Ref<BrowserXmppClient | null>,
  session: Ref<WaddleSession | null>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
) {
  const waddles = ref<SpaceSummary[]>([]);
  const channels = ref<ChannelSummary[]>([]);
  const members = ref<MemberSummary[]>([]);

  const activeSpaceId: Ref<string | null> = ref(null);
  const activeChannelId: Ref<string | null> = ref(null);

  const isLoadingStructure = ref(false);
  const isSubmitting = ref(false);

  let spaceRequestId = 0;
  let structureRequestId = 0;
  let memberRequestId = 0;

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

  const currentSpace = computed(() => (activeSpaceId.value && waddles.value[0]) ? waddles.value[0] : null);
  const isEmptyDeployment = computed(() =>
    waddles.value.length === 0 && channels.value.length === 0 && !isLoadingStructure.value,
  );

  const currentChannel = computed(
    () => channels.value.find((c) => c.id === activeChannelId.value) ?? null,
  );

  const currentRole = computed(() => {
    if (!session.value || !currentSpace.value) return null;
    return (
      members.value.find((m) => m.jid === session.value!.jid.split("/")[0])?.role ??
      currentSpace.value.role ??
      null
    );
  });

  const sortedSpaces = computed(() =>
    [...waddles.value].sort((a, b) =>
      a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
    ),
  );

  const sortedChannels = computed(() =>
    [...channels.value].sort(
      (a, b) =>
        (a.position ?? 0) - (b.position ?? 0) ||
        a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
    ),
  );

  const sortedMembers = computed(() => {
    const order = { owner: 0, admin: 1, member: 2, outcast: 3, none: 4 } as const;
    return [...members.value].sort(
      (a, b) =>
        (order[a.role] ?? 4) - (order[b.role] ?? 4) ||
        a.username.localeCompare(b.username, undefined, { sensitivity: "base" }),
    );
  });

  const canManageCommunity = computed(() =>
    ["owner", "admin"].includes(currentRole.value ?? ""),
  );

  const canManageChannels = computed(() =>
    isEmptyDeployment.value || ["owner", "admin"].includes(currentRole.value ?? ""),
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
      editChannelForm.value = {
        name: c.name,
        description: c.description ?? "",
        position: c.position ?? 0,
      };
    }
  });

  function resetForms() {
    createChannelForm.value = defaultCreateForm();
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

      const discoveredSpaces = topology.spaces.map((space) => ({
        name: space.name,
        role: null,
      }) satisfies SpaceSummary);
      waddles.value = discoveredSpaces;
      activeSpaceId.value = topology.spaces[0]?.id ?? null;

      const channelList: ChannelSummary[] = topology.rooms.map((c) => ({
        id: c.id,
        name: c.name,
        jid: c.jid,
        channel_type: c.channelType,
        position: c.position,
      }));

      channels.value = channelList;
      members.value = [];

      if (opts?.noChannelSelect) {
        activeChannelId.value = null;
        resetForms();
        return null;
      }

      const nextChannelId =
        preferredChannelId && channelList.some((c) => c.id === preferredChannelId)
          ? preferredChannelId
          : activeChannelId.value && channelList.some((c) => c.id === activeChannelId.value)
            ? activeChannelId.value
            : channelList[0]?.id ?? null;

      activeChannelId.value = nextChannelId;
      const nextChannel = channelList.find((channel) => channel.id === nextChannelId);
      if (nextChannelId && xmppClient.value) {
        const memberReqId = ++memberRequestId;
        try {
          const freshMembers = await xmppClient.value.listRoomMembers(nextChannelId, { roomJid: nextChannel?.jid });
          if (requestId === structureRequestId && memberReqId === memberRequestId) {
            members.value = freshMembers;
          }
        } catch (e) {
          if (requestId === structureRequestId && memberReqId === memberRequestId) {
            actionError.value = normalizeError(e);
          }
        }
      } else {
        members.value = [];
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

    try {
      const freshMembers = await xmppClient.value.listRoomMembers(channelId, { roomJid: channel?.jid });
      if (requestId !== memberRequestId) return;
      members.value = freshMembers;
    } catch (e) {
      if (requestId === memberRequestId) {
        actionError.value = normalizeError(e);
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

    const domain = jidDomain(session.value.jid);
    const mucServiceJid = `muc.${domain}`;
    const spacesServiceJid = `spaces.${domain}`;
    const nick = session.value.username;

    isSubmitting.value = true;
    clearActionError();

    try {
      if (form.intent === "muc") {
        if (!form.name.trim()) return undefined;

        const { roomJid } = await createMucRoom(xmppAgent, mucServiceJid, {
          roomLocalpart: form.name.trim().toLowerCase().replace(/\s+/g, "-"),
          nick,
          name: form.name.trim(),
          description: form.description.trim() || undefined,
          mucType: form.muc_type,
        });

        const channelId = roomJid.split("@")[0] ?? form.name.trim();
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

        const { node } = await createSpaceNode(xmppAgent, spacesServiceJid, {
          name: form.name.trim(),
          description: form.description.trim() || undefined,
        });

        createChannelForm.value = defaultCreateForm();
        await loadStructure(null, { noChannelSelect: true });
        return {
          intent: "space",
          spaceId: node,
          spaceName: form.name.trim(),
        } satisfies CreateChannelResult;
      }

      if (form.intent === "space-muc") {
        if (!form.name.trim() || !form.space_jid.trim()) return undefined;

        const spaceNode = form.space_jid.split("@")[0] ?? form.space_jid;
        const { roomJid } = await createMucInSpace(xmppAgent, mucServiceJid, spacesServiceJid, {
          roomLocalpart: form.name.trim().toLowerCase().replace(/\s+/g, "-"),
          nick,
          name: form.name.trim(),
          description: form.description.trim() || undefined,
          mucType: form.muc_type,
          spaceNode,
        });

        const channelId = roomJid.split("@")[0] ?? form.name.trim();
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
          mucServiceJid,
          spacesServiceJid,
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

        const channelId = roomJid.split("@")[0] ?? form.muc_name.trim();
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

  async function updateChannel() {
    actionError.value = "Channel editing is not available until the XMPP command exists.";
  }

  async function deleteChannel() {
    actionError.value = "Channel deletion is not available until the XMPP command exists.";
  }

  function clearData() {
    spaceRequestId++;
    structureRequestId++;
    memberRequestId++;
    waddles.value = [];
    channels.value = [];
    members.value = [];
    activeSpaceId.value = null;
    activeChannelId.value = null;
  }

  return {
    waddles,
    channels,
    members,
    activeSpaceId,
    activeChannelId,
    isLoadingStructure,
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
    updateWaddle,
    deleteWaddle,
    createChannel,
    updateChannel,
    deleteChannel,
    clearData,
    resetForms,
  };
}
