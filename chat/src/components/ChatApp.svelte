<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import { Drawer } from "@ark-ui/svelte/drawer";
  import LandingState from "./chat/LandingState.svelte";
  import CommunitiesPanel from "./chat/CommunitiesPanel.svelte";
  import ConversationPanel from "./chat/ConversationPanel.svelte";
  import AdminPanel from "./chat/AdminPanel.svelte";
  import { createServerAuth, type WaddleSession } from "../lib/server-auth";
  import {
    WaddleApi,
    type ChannelMessage,
    type ChannelSummary,
    type MemberSummary,
    type UserSearchResult,
    type WaddleSummary,
  } from "../lib/waddle-api";
  import {
    BrowserXmppClient,
    roomBareJidFor,
    type LiveRoomMessage,
    type XmppStatusSnapshot,
  } from "../lib/xmpp-client";
  import type {
    AdminTab,
    AppState,
    ChannelCreateFormData,
    ChannelEditFormData,
    CommunityFormData,
    EditableRole,
    TimelineMessage,
  } from "../lib/chat-ui";

  export let serverBaseUrl: string;

  const auth = createServerAuth(serverBaseUrl);

  let appState: AppState = "loading";
  let session: WaddleSession | null = null;
  let api: WaddleApi | null = null;
  let xmpp: BrowserXmppClient | null = null;
  let xmppStatus: XmppStatusSnapshot = {
    state: "offline",
    detail: "Live room offline",
  };

  let appError = "";
  let actionError = "";

  let isBootstrapping = false;
  let isLoadingStructure = false;
  let isLoadingMessages = false;
  let isSubmitting = false;
  let isSending = false;
  let isSearchingUsers = false;

  let waddles: WaddleSummary[] = [];
  let channels: ChannelSummary[] = [];
  let members: MemberSummary[] = [];
  let messages: TimelineMessage[] = [];

  let activeWaddleId: string | null = null;
  let activeChannelId: string | null = null;
  let adminTab: AdminTab = "rooms";
  let showMobileNav = false;
  let showMobileDetails = false;

  let timelineViewport: HTMLDivElement | null = null;
  let draft = "";

  let showCreateWaddle = false;
  let showCreateChannel = false;
  let showEditChannel = false;

  let createWaddleForm: CommunityFormData = {
    name: "",
    description: "",
    is_public: true,
  };

  let editWaddleForm: CommunityFormData = {
    name: "",
    description: "",
    is_public: true,
  };

  let createChannelForm: ChannelCreateFormData = {
    name: "",
    description: "",
    channel_type: "text",
    position: 0,
  };

  let editChannelForm: ChannelEditFormData = {
    name: "",
    description: "",
    position: 0,
  };

  let memberQuery = "";
  let newMemberRole: EditableRole = "member";
  let memberSearchResults: UserSearchResult[] = [];
  let memberSearchTimer: number | null = null;

  let currentWaddle: WaddleSummary | null = null;
  let currentChannel: ChannelSummary | null = null;
  let currentRole: MemberSummary["role"] | WaddleSummary["role"] | null = null;
  let currentRoomJid: string | null = null;
  let sortedWaddles: WaddleSummary[] = [];
  let sortedChannels: ChannelSummary[] = [];
  let sortedMembers: MemberSummary[] = [];
  let canManageCommunity = false;
  let canManageChannels = false;
  let canManageMembers = false;

  let structureRequestId = 0;
  let messageRequestId = 0;

  $: currentWaddle = waddles.find((item) => item.id === activeWaddleId) ?? null;
  $: currentChannel = channels.find((item) => item.id === activeChannelId) ?? null;
  $: currentRole =
    session && currentWaddle
      ? members.find((member) => member.user_id === session.user_id)?.role ?? currentWaddle.role ?? null
      : null;
  $: currentRoomJid =
    session && activeWaddleId && activeChannelId
      ? roomBareJidFor(session, activeWaddleId, activeChannelId)
      : null;
  $: sortedWaddles = [...waddles].sort((left, right) =>
    left.name.localeCompare(right.name, undefined, { sensitivity: "base" }),
  );
  $: sortedChannels = [...channels].sort(
    (left, right) =>
      left.position - right.position ||
      left.name.localeCompare(right.name, undefined, { sensitivity: "base" }),
  );
  $: sortedMembers = [...members].sort((left, right) => {
    const order = {
      owner: 0,
      admin: 1,
      moderator: 2,
      member: 3,
    } satisfies Record<MemberSummary["role"], number>;

    return (
      order[left.role] - order[right.role] ||
      left.username.localeCompare(right.username, undefined, { sensitivity: "base" })
    );
  });
  $: canManageCommunity = ["owner", "admin"].includes(currentRole ?? "");
  $: canManageChannels = ["owner", "admin", "moderator"].includes(currentRole ?? "");
  $: canManageMembers = ["owner", "admin"].includes(currentRole ?? "");

  $: if (currentWaddle) {
    editWaddleForm = {
      name: currentWaddle.name,
      description: currentWaddle.description ?? "",
      is_public: currentWaddle.is_public,
    };
  }

  $: if (currentChannel) {
    editChannelForm = {
      name: currentChannel.name,
      description: currentChannel.description ?? "",
      position: currentChannel.position,
    };
  }

  $: if (typeof window !== "undefined") {
    if (memberSearchTimer) {
      window.clearTimeout(memberSearchTimer);
      memberSearchTimer = null;
    }

    const query = memberQuery.trim();

    if (!query || !api || !canManageMembers || !activeWaddleId) {
      memberSearchResults = [];
      isSearchingUsers = false;
    } else {
      memberSearchTimer = window.setTimeout(async () => {
        isSearchingUsers = true;

        try {
          const response = await api.searchUsers(query);
          const existingIds = new Set(members.map((member) => member.user_id));
          memberSearchResults = response.users.filter((user) => !existingIds.has(user.id));
        } catch (value) {
          actionError = normalizeError(value);
        } finally {
          isSearchingUsers = false;
        }
      }, 220);
    }
  }

  function normalizeError(value: unknown) {
    return value instanceof Error ? value.message : "Something went wrong.";
  }

  function formatStamp(value: string) {
    return new Intl.DateTimeFormat("en", {
      hour: "2-digit",
      minute: "2-digit",
      month: "short",
      day: "numeric",
    }).format(new Date(value));
  }

  function toTimelineMessage(sessionValue: WaddleSession, message: ChannelMessage): TimelineMessage {
    return {
      id: message.id,
      author: message.author.display_name || message.author.username,
      body: message.content ?? "",
      createdAt: message.created_at,
      isSelf: message.author.user_id === sessionValue.user_id,
    };
  }

  function fromLiveMessage(sessionValue: WaddleSession, message: LiveRoomMessage): TimelineMessage {
    return {
      id: message.id,
      author: message.nick,
      body: message.body,
      createdAt: message.createdAt,
      isSelf: message.nick === sessionValue.username,
    };
  }

  function clearActionError() {
    actionError = "";
  }

  function resetForms() {
    createWaddleForm = {
      name: "",
      description: "",
      is_public: true,
    };
    createChannelForm = {
      name: "",
      description: "",
      channel_type: "text",
      position: channels.length,
    };
    memberQuery = "";
    memberSearchResults = [];
    newMemberRole = "member";
  }

  async function scrollToBottom() {
    await tick();
    timelineViewport?.scrollTo({
      top: timelineViewport.scrollHeight,
      behavior: "auto",
    });
  }

  function mergeLiveMessage(message: TimelineMessage) {
    if (messages.some((entry) => entry.id === message.id)) {
      return;
    }

    messages = [...messages, message];
    void scrollToBottom();
  }

  async function ensureXmpp() {
    if (!session || xmpp) {
      return;
    }

    xmpp = new BrowserXmppClient(
      session,
      (message) => {
        if (!currentRoomJid || message.roomJid !== currentRoomJid || message.type !== "message") {
          return;
        }

        mergeLiveMessage(fromLiveMessage(session, message));
      },
      (status) => {
        xmppStatus = status;
      },
    );

    try {
      await xmpp.connect();
    } catch (value) {
      xmppStatus = {
        state: "error",
        detail: normalizeError(value),
      };
    }
  }

  async function loadMessages(waddleId: string, channelId: string) {
    if (!api || !session) {
      return;
    }

    const requestId = ++messageRequestId;
    isLoadingMessages = true;
    clearActionError();

    try {
      await ensureXmpp();

      const [history] = await Promise.all([
        api.listMessages(waddleId, channelId),
        xmpp?.switchRoom(waddleId, channelId),
      ]);

      if (requestId !== messageRequestId || activeWaddleId !== waddleId || activeChannelId !== channelId) {
        return;
      }

      messages = history.messages.reverse().map((message) => toTimelineMessage(session, message));
      await scrollToBottom();
    } catch (value) {
      if (requestId === messageRequestId) {
        actionError = normalizeError(value);
      }
    } finally {
      if (requestId === messageRequestId) {
        isLoadingMessages = false;
      }
    }
  }

  async function selectChannel(channelId: string) {
    if (!activeWaddleId) {
      return;
    }

    activeChannelId = channelId;
    messages = [];
    await loadMessages(activeWaddleId, channelId);
  }

  async function selectWaddleFromNav(waddleId: string) {
    await loadWaddles(waddleId);
    showMobileNav = false;
  }

  async function selectChannelFromNav(channelId: string) {
    await selectChannel(channelId);
    showMobileNav = false;
  }

  async function loadStructure(waddleId: string, preferredChannelId?: string | null) {
    if (!api) {
      return;
    }

    const requestId = ++structureRequestId;
    isLoadingStructure = true;
    clearActionError();

    try {
      const [channelResponse, memberResponse] = await Promise.all([
        api.listChannels(waddleId),
        api.listMembers(waddleId),
      ]);

      if (requestId !== structureRequestId || activeWaddleId !== waddleId) {
        return;
      }

      channels = channelResponse.channels;
      members = memberResponse.members;

      const nextChannelId =
        preferredChannelId && channelResponse.channels.some((channel) => channel.id === preferredChannelId)
          ? preferredChannelId
          : activeChannelId && channelResponse.channels.some((channel) => channel.id === activeChannelId)
            ? activeChannelId
            : channelResponse.channels[0]?.id ?? null;

      activeChannelId = nextChannelId;
      resetForms();

      if (nextChannelId) {
        await loadMessages(waddleId, nextChannelId);
      }
    } catch (value) {
      if (requestId === structureRequestId) {
        actionError = normalizeError(value);
      }
    } finally {
      if (requestId === structureRequestId) {
        isLoadingStructure = false;
      }
    }
  }

  async function loadWaddles(preferredId?: string | null) {
    if (!api) {
      return;
    }

    const response = await api.listWaddles();
    waddles = response.waddles;

    const nextId =
      preferredId && response.waddles.some((item) => item.id === preferredId)
        ? preferredId
        : activeWaddleId && response.waddles.some((item) => item.id === activeWaddleId)
          ? activeWaddleId
          : response.waddles[0]?.id ?? null;

    activeWaddleId = nextId;

    if (nextId) {
      await loadStructure(nextId);
    } else {
      channels = [];
      members = [];
      messages = [];
      activeChannelId = null;
    }
  }

  async function bootstrap() {
    isBootstrapping = true;
    appState = "loading";
    appError = "";
    actionError = "";

    try {
      const loadedSession = await auth.getSession();

      if (!loadedSession || loadedSession.is_expired) {
        session = null;
        api = null;
        appState = "signed-out";
        return;
      }

      session = loadedSession;
      api = new WaddleApi(serverBaseUrl, loadedSession.session_id);
      appState = "ready";
      await loadWaddles();
    } catch (value) {
      appError = normalizeError(value);
      appState = "error";
    } finally {
      isBootstrapping = false;
    }
  }

  function handleLogin() {
    window.location.href = auth.loginUrl(window.location.href);
  }

  async function handleLogout() {
    try {
      await auth.logout();
      await xmpp?.disconnect();
      xmpp = null;
      session = null;
      api = null;
      xmppStatus = {
        state: "offline",
        detail: "Live room offline",
      };
      appState = "signed-out";
      actionError = "";
      waddles = [];
      channels = [];
      members = [];
      messages = [];
      activeWaddleId = null;
      activeChannelId = null;
    } catch (value) {
      actionError = normalizeError(value);
    }
  }

  async function createWaddle() {
    if (!api || !createWaddleForm.name.trim()) {
      return;
    }

    isSubmitting = true;
    clearActionError();

    try {
      const created = await api.createWaddle({
        name: createWaddleForm.name.trim(),
        description: createWaddleForm.description.trim() || undefined,
        is_public: createWaddleForm.is_public,
      });

      showCreateWaddle = false;
      resetForms();
      await loadWaddles(created.id);
      adminTab = "rooms";
    } catch (value) {
      actionError = normalizeError(value);
    } finally {
      isSubmitting = false;
    }
  }

  async function updateWaddle() {
    if (!api || !currentWaddle || !editWaddleForm.name.trim()) {
      return;
    }

    isSubmitting = true;
    clearActionError();

    try {
      await api.updateWaddle(currentWaddle.id, {
        name: editWaddleForm.name.trim(),
        description: editWaddleForm.description.trim() || undefined,
        is_public: editWaddleForm.is_public,
      });

      await loadWaddles(currentWaddle.id);
    } catch (value) {
      actionError = normalizeError(value);
    } finally {
      isSubmitting = false;
    }
  }

  async function deleteCurrentWaddle() {
    if (!api || !currentWaddle) {
      return;
    }

    if (!window.confirm(`Delete ${currentWaddle.name}?`)) {
      return;
    }

    isSubmitting = true;
    clearActionError();

    try {
      await api.deleteWaddle(currentWaddle.id);
      await loadWaddles();
    } catch (value) {
      actionError = normalizeError(value);
    } finally {
      isSubmitting = false;
    }
  }

  async function createChannel() {
    if (!api || !activeWaddleId || !createChannelForm.name.trim()) {
      return;
    }

    isSubmitting = true;
    clearActionError();

    try {
      const created = await api.createChannel(activeWaddleId, {
        name: createChannelForm.name.trim(),
        description: createChannelForm.description.trim() || undefined,
        channel_type: createChannelForm.channel_type,
        position: createChannelForm.position,
      });

      showCreateChannel = false;
      createChannelForm = {
        name: "",
        description: "",
        channel_type: "text",
        position: channels.length + 1,
      };
      await loadStructure(activeWaddleId, created.id);
    } catch (value) {
      actionError = normalizeError(value);
    } finally {
      isSubmitting = false;
    }
  }

  async function updateChannel() {
    if (!api || !activeWaddleId || !currentChannel || !editChannelForm.name.trim()) {
      return;
    }

    isSubmitting = true;
    clearActionError();

    try {
      await api.updateChannel(activeWaddleId, currentChannel.id, {
        name: editChannelForm.name.trim(),
        description: editChannelForm.description.trim() || undefined,
        position: editChannelForm.position,
      });

      showEditChannel = false;
      await loadStructure(activeWaddleId, currentChannel.id);
    } catch (value) {
      actionError = normalizeError(value);
    } finally {
      isSubmitting = false;
    }
  }

  async function deleteCurrentChannel() {
    if (!api || !activeWaddleId || !currentChannel) {
      return;
    }

    if (!window.confirm(`Delete #${currentChannel.name}?`)) {
      return;
    }

    isSubmitting = true;
    clearActionError();

    try {
      const deletedId = currentChannel.id;
      await api.deleteChannel(activeWaddleId, deletedId);
      showEditChannel = false;
      await loadStructure(activeWaddleId, channels.find((channel) => channel.id !== deletedId)?.id ?? null);
    } catch (value) {
      actionError = normalizeError(value);
    } finally {
      isSubmitting = false;
    }
  }

  async function addMember(userId: string) {
    if (!api || !activeWaddleId) {
      return;
    }

    isSubmitting = true;
    clearActionError();

    try {
      await api.addMember(activeWaddleId, {
        user_id: userId,
        role: newMemberRole,
      });

      memberQuery = "";
      memberSearchResults = [];
      await loadStructure(activeWaddleId, activeChannelId);
      adminTab = "people";
    } catch (value) {
      actionError = normalizeError(value);
    } finally {
      isSubmitting = false;
    }
  }

  async function updateMemberRole(member: MemberSummary, role: EditableRole) {
    if (!api || !activeWaddleId || member.role === "owner") {
      return;
    }

    clearActionError();

    try {
      await api.updateMemberRole(activeWaddleId, member.user_id, role);
      await loadStructure(activeWaddleId, activeChannelId);
    } catch (value) {
      actionError = normalizeError(value);
    }
  }

  async function removeMember(member: MemberSummary) {
    if (!api || !activeWaddleId || member.role === "owner") {
      return;
    }

    if (!window.confirm(`Remove ${member.username}?`)) {
      return;
    }

    clearActionError();

    try {
      await api.removeMember(activeWaddleId, member.user_id);
      await loadStructure(activeWaddleId, activeChannelId);
    } catch (value) {
      actionError = normalizeError(value);
    }
  }

  async function sendMessage() {
    if (!xmpp || !activeWaddleId || !activeChannelId || !draft.trim()) {
      return;
    }

    isSending = true;
    clearActionError();

    try {
      await xmpp.sendGroupMessage(activeWaddleId, activeChannelId, draft);
      draft = "";
    } catch (value) {
      actionError = normalizeError(value);
    } finally {
      isSending = false;
    }
  }

  onMount(() => {
    void bootstrap();
  });

  onDestroy(() => {
    if (memberSearchTimer) {
      window.clearTimeout(memberSearchTimer);
    }

    void xmpp?.disconnect().catch(() => undefined);
  });
</script>

{#if appState === "loading"}
  <LandingState title="Checking session." />
{:else if appState === "signed-out"}
  <LandingState
    title="Sign in with Colony."
    copy="Rooms, membership, and live chat in one compact client."
    actionLabel="Log in"
    onAction={handleLogin}
  />
{:else if appState === "error"}
  <LandingState title="Server unavailable." copy={appError} actionLabel="Try again" onAction={bootstrap} />
{:else}
  <section class="mobile-header">
    <button type="button" on:click={() => (showMobileNav = true)}>Menu</button>
    <div class="mobile-header__title">
      <strong>{currentChannel ? `#${currentChannel.name}` : currentWaddle?.name ?? "Waddle Chat"}</strong>
      <span>{currentWaddle?.name ?? session?.username}</span>
    </div>
    <button type="button" on:click={() => (showMobileDetails = true)}>Details</button>
  </section>

  <Drawer.Root bind:open={showMobileNav} placement="left" modal>
    <Drawer.Backdrop class="mobile-drawer-backdrop" />
    <Drawer.Positioner class="mobile-drawer-positioner mobile-drawer-positioner--left">
      <Drawer.Content class="mobile-drawer">
        <div class="mobile-drawer__header">
          <Drawer.Title>Navigation</Drawer.Title>
          <Drawer.CloseTrigger>Close</Drawer.CloseTrigger>
        </div>
        <CommunitiesPanel
          {session}
          bind:showCreateWaddle
          bind:createWaddleForm
          {sortedWaddles}
          {sortedChannels}
          {currentWaddle}
          {activeWaddleId}
          {activeChannelId}
          {canManageChannels}
          {isLoadingStructure}
          {isSubmitting}
          onSelectWaddle={selectWaddleFromNav}
          onSelectChannel={selectChannelFromNav}
          onCreate={createWaddle}
        />
      </Drawer.Content>
    </Drawer.Positioner>
  </Drawer.Root>

  <Drawer.Root bind:open={showMobileDetails} placement="right" modal>
    <Drawer.Backdrop class="mobile-drawer-backdrop" />
    <Drawer.Positioner class="mobile-drawer-positioner mobile-drawer-positioner--right">
      <Drawer.Content class="mobile-drawer">
        <div class="mobile-drawer__header">
          <Drawer.Title>Details</Drawer.Title>
          <Drawer.CloseTrigger>Close</Drawer.CloseTrigger>
        </div>
        <AdminPanel
          bind:adminTab
          bind:showCreateChannel
          bind:showEditChannel
          bind:createChannelForm
          bind:editChannelForm
          bind:editWaddleForm
          bind:memberQuery
          bind:newMemberRole
          {session}
          {currentRole}
          {currentWaddle}
          {currentChannel}
          {sortedChannels}
          {sortedMembers}
          {activeChannelId}
          {memberSearchResults}
          {canManageCommunity}
          {canManageChannels}
          {canManageMembers}
          {isLoadingStructure}
          {isSearchingUsers}
          {isSubmitting}
          onCreateChannel={createChannel}
          onUpdateChannel={updateChannel}
          onDeleteChannel={deleteCurrentChannel}
          onAddMember={addMember}
          onUpdateMemberRole={updateMemberRole}
          onRemoveMember={removeMember}
          onUpdateWaddle={updateWaddle}
          onDeleteWaddle={deleteCurrentWaddle}
        />
      </Drawer.Content>
    </Drawer.Positioner>
  </Drawer.Root>

  <section class="shell">
    <CommunitiesPanel
      {session}
      bind:showCreateWaddle
      bind:createWaddleForm
      {sortedWaddles}
      {sortedChannels}
      {currentWaddle}
      {activeWaddleId}
      {activeChannelId}
      {canManageChannels}
      {isLoadingStructure}
      {isSubmitting}
      onSelectWaddle={selectWaddleFromNav}
      onSelectChannel={selectChannelFromNav}
      onCreate={createWaddle}
    />

    <ConversationPanel
      bind:draft
      bind:timelineViewport
      {currentWaddle}
      {currentChannel}
      {currentRoomJid}
      {session}
      {xmppStatus}
      {actionError}
      {messages}
      {isLoadingMessages}
      {isBootstrapping}
      {isSending}
      {formatStamp}
      onLogout={handleLogout}
      onSend={sendMessage}
    />

    <AdminPanel
      bind:adminTab
      bind:showCreateChannel
      bind:showEditChannel
      bind:createChannelForm
      bind:editChannelForm
      bind:editWaddleForm
      bind:memberQuery
      bind:newMemberRole
      {session}
      {currentRole}
      {currentWaddle}
      {currentChannel}
      {sortedChannels}
      {sortedMembers}
      {activeChannelId}
      {memberSearchResults}
      {canManageCommunity}
      {canManageChannels}
      {canManageMembers}
      {isLoadingStructure}
      {isSearchingUsers}
      {isSubmitting}
      onCreateChannel={createChannel}
      onUpdateChannel={updateChannel}
      onDeleteChannel={deleteCurrentChannel}
      onAddMember={addMember}
      onUpdateMemberRole={updateMemberRole}
      onRemoveMember={removeMember}
      onUpdateWaddle={updateWaddle}
      onDeleteWaddle={deleteCurrentWaddle}
    />
  </section>
{/if}
