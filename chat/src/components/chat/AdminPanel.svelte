<script lang="ts">
  import { Tabs } from "@ark-ui/svelte/tabs";
  import RoomsAdmin from "./RoomsAdmin.svelte";
  import PeopleAdmin from "./PeopleAdmin.svelte";
  import SettingsAdmin from "./SettingsAdmin.svelte";
  import type {
    AdminTab,
    ChannelCreateFormData,
    ChannelEditFormData,
    CommunityFormData,
    EditableRole,
  } from "../../lib/chat-ui";
  import type { WaddleSession } from "../../lib/server-auth";
  import type { ChannelSummary, MemberSummary, UserSearchResult, WaddleSummary } from "../../lib/waddle-api";

  export let adminTab: AdminTab = "rooms";
  export let session: WaddleSession | null = null;
  export let currentRole: MemberSummary["role"] | WaddleSummary["role"] | null = null;
  export let currentWaddle: WaddleSummary | null = null;
  export let currentChannel: ChannelSummary | null = null;
  export let sortedChannels: ChannelSummary[] = [];
  export let sortedMembers: MemberSummary[] = [];
  export let activeChannelId: string | null = null;
  export let showCreateChannel = false;
  export let showEditChannel = false;
  export let createChannelForm: ChannelCreateFormData = {
    name: "",
    description: "",
    channel_type: "text",
    position: 0,
  };
  export let editChannelForm: ChannelEditFormData = {
    name: "",
    description: "",
    position: 0,
  };
  export let editWaddleForm: CommunityFormData = {
    name: "",
    description: "",
    is_public: true,
  };
  export let memberQuery = "";
  export let newMemberRole: EditableRole = "member";
  export let memberSearchResults: UserSearchResult[] = [];
  export let canManageCommunity = false;
  export let canManageChannels = false;
  export let canManageMembers = false;
  export let isLoadingStructure = false;
  export let isSearchingUsers = false;
  export let isSubmitting = false;
  export let onCreateChannel: () => void | Promise<void>;
  export let onUpdateChannel: () => void | Promise<void>;
  export let onDeleteChannel: () => void | Promise<void>;
  export let onAddMember: (userId: string) => void | Promise<void>;
  export let onUpdateMemberRole: (member: MemberSummary, role: EditableRole) => void | Promise<void>;
  export let onRemoveMember: (member: MemberSummary) => void | Promise<void>;
  export let onUpdateWaddle: () => void | Promise<void>;
  export let onDeleteWaddle: () => void | Promise<void>;
</script>

<aside class="panel details-panel">
  <header class="panel__header">
    <p class="eyebrow">Details</p>
    <h2>{currentWaddle?.name ?? "Community details"}</h2>
    <p class="muted">
      {#if currentChannel}
        Managing #{currentChannel.name}
      {:else}
        Select a room to inspect it here
      {/if}
    </p>
  </header>

  <section class="details-summary">
    <div class="summary-card">
      <span class="summary-card__label">Signed in</span>
      <strong>{session?.username ?? "Unknown user"}</strong>
      <span class="summary-card__meta">{session?.jid ?? "No JID"}</span>
    </div>

    <div class="summary-card">
      <span class="summary-card__label">Current role</span>
      <strong>{currentRole ?? "member"}</strong>
      <span class="summary-card__meta">
        {sortedMembers.length} people · {sortedChannels.length} rooms
      </span>
    </div>

    <div class="summary-card">
      <span class="summary-card__label">Focused room</span>
      <strong>{currentChannel ? `#${currentChannel.name}` : "No room selected"}</strong>
      <span class="summary-card__meta">
        {currentChannel?.description || "Switch rooms from the navigation rail."}
      </span>
    </div>
  </section>

  <Tabs.Root bind:value={adminTab}>
    <Tabs.List class="actions" aria-label="Admin sections">
      <Tabs.Trigger value="rooms">Rooms</Tabs.Trigger>
      <Tabs.Trigger value="people">People</Tabs.Trigger>
      <Tabs.Trigger value="settings">Settings</Tabs.Trigger>
    </Tabs.List>

    <Tabs.Content value="rooms">
      <RoomsAdmin
        bind:showCreateChannel
        bind:showEditChannel
        bind:createChannelForm
        bind:editChannelForm
        {sortedChannels}
        {activeChannelId}
        {currentChannel}
        {canManageChannels}
        {isLoadingStructure}
        {isSubmitting}
        onCreateChannel={onCreateChannel}
        onUpdateChannel={onUpdateChannel}
        onDeleteChannel={onDeleteChannel}
      />
    </Tabs.Content>

    <Tabs.Content value="people">
      <PeopleAdmin
        bind:memberQuery
        bind:newMemberRole
        {sortedMembers}
        {memberSearchResults}
        {canManageMembers}
        {isSearchingUsers}
        {isSubmitting}
        onAddMember={onAddMember}
        onUpdateMemberRole={onUpdateMemberRole}
        onRemoveMember={onRemoveMember}
      />
    </Tabs.Content>

    <Tabs.Content value="settings">
      <SettingsAdmin
        bind:editWaddleForm
        {currentWaddle}
        {canManageCommunity}
        {isSubmitting}
        onUpdateWaddle={onUpdateWaddle}
        onDeleteWaddle={onDeleteWaddle}
      />
    </Tabs.Content>
  </Tabs.Root>
</aside>
