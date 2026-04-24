import { ref, computed, watch, type Ref } from "vue";
import type { SpaceSummary, ChannelSummary, MemberSummary } from "@/lib/chat-types";
import type { WaddleSession } from "@/lib/server-auth";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { CommunityFormData, ChannelCreateFormData, ChannelEditFormData } from "@/lib/chat-ui";
import { executeCreateChannelCommand } from "@/lib/xmpp/commands";
import { jidDomain } from "@/lib/xmpp/jid";

interface LoadSpaceOptions {
  loadStructure?: boolean;
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

  const editWaddleForm = ref<CommunityFormData>({
    name: "",
    description: "",
    is_public: true,
  });

  const createChannelForm = ref<ChannelCreateFormData>({
    name: "",
    description: "",
    channel_type: "text",
    position: 0,
  });

  const editChannelForm = ref<ChannelEditFormData>({
    name: "",
    description: "",
    position: 0,
  });

  const currentSpace = computed(() => (activeSpaceId.value && waddles.value[0]) ? waddles.value[0] : null);

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
    ["owner", "admin"].includes(currentRole.value ?? ""),
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
    createChannelForm.value = {
      name: "",
      description: "",
      channel_type: "text",
      position: channels.value.length,
    };
  }

  async function loadStructure(preferredChannelId?: string | null): Promise<string | null> {
    if (!xmppClient.value) return null;

    const requestId = ++structureRequestId;
    isLoadingStructure.value = true;
    clearActionError();

    try {
      const discoveredChannels = await xmppClient.value.discoverSpaceChannels();

      if (requestId !== structureRequestId) {
        return null;
      }

      activeSpaceId.value = "space";

      const channelList: ChannelSummary[] = discoveredChannels.map((c) => ({
        id: c.id,
        name: c.name,
        channel_type: c.channelType,
        position: c.position,
      }));

      channels.value = channelList;
      members.value = [];

      const nextChannelId =
        preferredChannelId && channelList.some((c) => c.id === preferredChannelId)
          ? preferredChannelId
          : activeChannelId.value && channelList.some((c) => c.id === activeChannelId.value)
            ? activeChannelId.value
            : channelList[0]?.id ?? null;

      activeChannelId.value = nextChannelId;
      members.value = nextChannelId && xmppClient.value
        ? await xmppClient.value.listRoomMembers(nextChannelId)
        : [];
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

    const canonical: SpaceSummary = {
      name: "Waddle",
      role: "owner",
    };
    waddles.value = [canonical];
    const nextId = activeSpaceId.value ?? "space";
    activeSpaceId.value = nextId;

    if (options.loadStructure !== false) {
      return loadStructure();
    } else {
      return null;
    }
  }

  async function updateWaddle() {
    actionError.value = "Space settings are managed by the server configuration.";
  }

  async function deleteWaddle() {
    actionError.value = "Deleting the configured space is not available in the XMPP-only client.";
  }

  async function createChannel() {
    if (!xmppClient.value || !session.value || !createChannelForm.value.name.trim()) return undefined;

    isSubmitting.value = true;
    clearActionError();

    try {
      const desc = createChannelForm.value.description.trim();
      const serverJid = jidDomain(session.value.jid);
      
      const xmppAgent = xmppClient.value.agent;
      if (!xmppAgent) {
        actionError.value = "XMPP connection not available";
        return undefined;
      }
      
      const result = await executeCreateChannelCommand(
        xmppAgent,
        serverJid,
        {
          name: createChannelForm.value.name.trim(),
          description: desc || undefined,
          channelType: createChannelForm.value.channel_type as "text" | "forum",
          position: createChannelForm.value.position,
        },
      );

      if (!result.success) {
        actionError.value = result.error ?? "Failed to create channel";
        return undefined;
      }

      // Capture created channel data before resetting the form
      const createdChannel = {
        id: result.channelId!,
        name: createChannelForm.value.name.trim(),
        channel_type: createChannelForm.value.channel_type,
      };

      createChannelForm.value = {
        name: "",
        description: "",
        channel_type: "text",
        position: channels.value.length + 1,
      };

      // Refresh channel discovery to show the new channel
      await loadStructure(result.channelId ?? null);
      
      return createdChannel;
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
    updateWaddle,
    deleteWaddle,
    createChannel,
    updateChannel,
    deleteChannel,
    clearData,
    resetForms,
  };
}
