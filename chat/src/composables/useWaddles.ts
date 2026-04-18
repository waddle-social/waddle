import { ref, computed, watch, type Ref } from "vue";
import type {
  WaddleApi,
  WaddleSummary,
  ChannelSummary,
  MemberSummary,
} from "@/lib/waddle-api";
import type { WaddleSession } from "@/lib/server-auth";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { CommunityFormData, ChannelCreateFormData, ChannelEditFormData } from "@/lib/chat-ui";
import { executeCreateChannelCommand } from "@/lib/xmpp/commands";
import { jidDomain } from "@/lib/xmpp/jid";

interface LoadWaddlesOptions {
  loadStructure?: boolean;
}

export function useWaddles(
  api: Ref<WaddleApi | null>,
  xmppClient: Ref<BrowserXmppClient | null>,
  session: Ref<WaddleSession | null>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
) {
  const waddles = ref<WaddleSummary[]>([]);
  const publicWaddles = ref<WaddleSummary[]>([]);
  const channels = ref<ChannelSummary[]>([]);
  const members = ref<MemberSummary[]>([]);

  const activeWaddleId: Ref<string | null> = ref(null);
  const activeChannelId: Ref<string | null> = ref(null);

  const isLoadingStructure = ref(false);
  const isLoadingPublicWaddles = ref(false);
  const isSubmitting = ref(false);
  const joiningPublicWaddleId = ref<string | null>(null);

  let waddleRequestId = 0;
  let structureRequestId = 0;
  let publicWaddlesRequestId = 0;

  const createWaddleForm = ref<CommunityFormData>({
    name: "",
    description: "",
    is_public: true,
  });

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

  const currentWaddle = computed(
    () => waddles.value.find((w) => w.id === activeWaddleId.value) ?? null,
  );

  const currentChannel = computed(
    () => channels.value.find((c) => c.id === activeChannelId.value) ?? null,
  );

  const currentRole = computed(() => {
    if (!session.value || !currentWaddle.value) return null;
    return (
      members.value.find((m) => m.user_id === session.value!.user_id)?.role ??
      currentWaddle.value.role ??
      null
    );
  });

  const sortedWaddles = computed(() =>
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
    const order = { owner: 0, admin: 1, moderator: 2, member: 3 } as const;
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
    ["owner", "admin", "moderator"].includes(currentRole.value ?? ""),
  );

  const canManageMembers = computed(() =>
    ["owner", "admin"].includes(currentRole.value ?? ""),
  );

  watch(currentWaddle, (w) => {
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
    createWaddleForm.value = { name: "", description: "", is_public: true };
    createChannelForm.value = {
      name: "",
      description: "",
      channel_type: "text",
      position: channels.value.length,
    };
  }

  async function loadStructure(
    waddleId: string,
    preferredChannelId?: string | null,
  ): Promise<string | null> {
    if (!api.value || !xmppClient.value) return null;

    const requestId = ++structureRequestId;
    isLoadingStructure.value = true;
    clearActionError();

    try {
      const [discoveredChannels, memberRes] = await Promise.all([
        xmppClient.value.discoverChannels(waddleId),
        api.value.listMembers(waddleId),
      ]);

      if (requestId !== structureRequestId || activeWaddleId.value !== waddleId) {
        return null;
      }

      const channelList: ChannelSummary[] = discoveredChannels.map((c) => ({
        id: c.id,
        name: c.name,
        channel_type: c.channelType,
        position: c.position,
      }));

      channels.value = channelList;
      members.value = memberRes.members;

      const nextChannelId =
        preferredChannelId && channelList.some((c) => c.id === preferredChannelId)
          ? preferredChannelId
          : activeChannelId.value && channelList.some((c) => c.id === activeChannelId.value)
            ? activeChannelId.value
            : channelList[0]?.id ?? null;

      activeChannelId.value = nextChannelId;
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

  async function loadWaddles(
    preferredId?: string | null,
    options: LoadWaddlesOptions = {},
  ) {
    if (!xmppClient.value) return null;

    const requestId = ++waddleRequestId;
    const discovered = await xmppClient.value.discoverWaddles();
    if (requestId !== waddleRequestId) return null;

    const waddleList: WaddleSummary[] = discovered.map((w) => ({
      id: w.id,
      name: w.name,
      is_public: w.isPublic,
    }));
    waddles.value = waddleList;

    const nextId =
      preferredId && waddleList.some((w) => w.id === preferredId)
        ? preferredId
        : activeWaddleId.value && waddleList.some((w) => w.id === activeWaddleId.value)
          ? activeWaddleId.value
          : waddleList[0]?.id ?? null;

    activeWaddleId.value = nextId;

    if (nextId && options.loadStructure !== false) {
      return loadStructure(nextId);
    } else {
      if (!nextId) {
        channels.value = [];
        members.value = [];
        activeChannelId.value = null;
      }
      return null;
    }
  }

  async function loadPublicWaddles(query?: string) {
    if (!api.value) return;

    const requestId = ++publicWaddlesRequestId;
    isLoadingPublicWaddles.value = true;
    clearActionError();
    try {
      const response = await api.value.listPublicWaddles(query);
      if (requestId === publicWaddlesRequestId) {
        publicWaddles.value = response.waddles;
      }
    } catch (e) {
      if (requestId === publicWaddlesRequestId) {
        actionError.value = normalizeError(e);
      }
    } finally {
      if (requestId === publicWaddlesRequestId) {
        isLoadingPublicWaddles.value = false;
      }
    }
  }

  async function joinPublicWaddle(
    waddleId: string,
  ): Promise<{ waddleId: string; channelId: string | null } | null> {
    if (!api.value) return null;

    joiningPublicWaddleId.value = waddleId;
    clearActionError();
    try {
      await api.value.joinWaddle(waddleId);
      const channelId = (await loadWaddles(waddleId)) ?? null;
      return { waddleId, channelId };
    } catch (e) {
      actionError.value = normalizeError(e);
      return null;
    } finally {
      joiningPublicWaddleId.value = null;
    }
  }

  async function createWaddle(): Promise<ReturnType<WaddleApi["createWaddle"]> extends Promise<infer T> ? T | undefined : never> {
    if (!api.value || !createWaddleForm.value.name.trim()) return undefined;

    isSubmitting.value = true;
    clearActionError();

    try {
      const desc = createWaddleForm.value.description.trim();
      const created = await api.value.createWaddle({
        name: createWaddleForm.value.name.trim(),
        ...(desc ? { description: desc } : {}),
        is_public: createWaddleForm.value.is_public,
      });
      resetForms();
      await loadWaddles(created.id);
      return created;
    } catch (e) {
      actionError.value = normalizeError(e);
      return undefined;
    } finally {
      isSubmitting.value = false;
    }
  }

  async function updateWaddle() {
    if (!api.value || !currentWaddle.value || !editWaddleForm.value.name.trim()) return;

    isSubmitting.value = true;
    clearActionError();

    try {
      const desc = editWaddleForm.value.description.trim();
      await api.value.updateWaddle(currentWaddle.value.id, {
        name: editWaddleForm.value.name.trim(),
        ...(desc ? { description: desc } : {}),
        is_public: editWaddleForm.value.is_public,
      });
      await loadWaddles(currentWaddle.value.id);
    } catch (e) {
      actionError.value = normalizeError(e);
    } finally {
      isSubmitting.value = false;
    }
  }

  async function deleteWaddle() {
    if (!api.value || !currentWaddle.value) return;

    isSubmitting.value = true;
    clearActionError();

    try {
      await api.value.deleteWaddle(currentWaddle.value.id);
      await loadWaddles();
    } catch (e) {
      actionError.value = normalizeError(e);
    } finally {
      isSubmitting.value = false;
    }
  }

  async function createChannel() {
    const waddleId = activeWaddleId.value;
    if (!xmppClient.value || !session.value || !waddleId || !createChannelForm.value.name.trim()) return undefined;

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
          waddleId,
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
      await loadStructure(waddleId, result.channelId ?? null);
      
      return createdChannel;
    } catch (e) {
      actionError.value = normalizeError(e);
      return undefined;
    } finally {
      isSubmitting.value = false;
    }
  }

  async function updateChannel() {
    const waddleId = activeWaddleId.value;
    const channelId = currentChannel.value?.id;
    if (!api.value || !waddleId || !channelId || !editChannelForm.value.name.trim()) return;

    isSubmitting.value = true;
    clearActionError();

    try {
      const desc = editChannelForm.value.description.trim();
      await api.value.updateChannel(waddleId, channelId, {
        name: editChannelForm.value.name.trim(),
        ...(desc ? { description: desc } : {}),
        position: editChannelForm.value.position,
      });
      await loadStructure(waddleId, channelId);
    } catch (e) {
      actionError.value = normalizeError(e);
    } finally {
      isSubmitting.value = false;
    }
  }

  async function deleteChannel() {
    const waddleId = activeWaddleId.value;
    const channelId = currentChannel.value?.id;
    if (!api.value || !waddleId || !channelId) return;

    isSubmitting.value = true;
    clearActionError();

    try {
      await api.value.deleteChannel(waddleId, channelId);
      await loadStructure(
        waddleId,
        channels.value.find((c) => c.id !== channelId)?.id ?? null,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    } finally {
      isSubmitting.value = false;
    }
  }

  function clearData() {
    waddleRequestId++;
    structureRequestId++;
    publicWaddlesRequestId++;
    waddles.value = [];
    publicWaddles.value = [];
    channels.value = [];
    members.value = [];
    isLoadingPublicWaddles.value = false;
    joiningPublicWaddleId.value = null;
    activeWaddleId.value = null;
    activeChannelId.value = null;
  }

  return {
    waddles,
    publicWaddles,
    channels,
    members,
    activeWaddleId,
    activeChannelId,
    isLoadingStructure,
    isLoadingPublicWaddles,
    isSubmitting,
    joiningPublicWaddleId,
    createWaddleForm,
    editWaddleForm,
    createChannelForm,
    editChannelForm,
    currentWaddle,
    currentChannel,
    currentRole,
    sortedWaddles,
    sortedChannels,
    sortedMembers,
    canManageCommunity,
    canManageChannels,
    canManageMembers,
    loadWaddles,
    loadPublicWaddles,
    loadStructure,
    joinPublicWaddle,
    createWaddle,
    updateWaddle,
    deleteWaddle,
    createChannel,
    updateChannel,
    deleteChannel,
    clearData,
    resetForms,
  };
}
