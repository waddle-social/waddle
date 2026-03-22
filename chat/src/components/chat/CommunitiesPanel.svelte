<script lang="ts">
  import { Checkbox } from "@ark-ui/svelte/checkbox";
  import { Dialog } from "@ark-ui/svelte/dialog";
  import { Field } from "@ark-ui/svelte/field";
  import type { WaddleSession } from "../../lib/server-auth";
  import type { CommunityFormData } from "../../lib/chat-ui";
  import type { ChannelSummary, WaddleSummary } from "../../lib/waddle-api";

  export let session: WaddleSession | null = null;
  export let sortedWaddles: WaddleSummary[] = [];
  export let sortedChannels: ChannelSummary[] = [];
  export let currentWaddle: WaddleSummary | null = null;
  export let activeWaddleId: string | null = null;
  export let activeChannelId: string | null = null;
  export let showCreateWaddle = false;
  export let createWaddleForm: CommunityFormData = {
    name: "",
    description: "",
    is_public: true,
  };
  export let canManageChannels = false;
  export let isSubmitting = false;
  export let isLoadingStructure = false;
  export let onSelectWaddle: (id: string) => void | Promise<void>;
  export let onSelectChannel: (id: string) => void | Promise<void>;
  export let onCreate: () => void | Promise<void>;
</script>

<aside class="panel nav-panel">
  <header class="nav-panel__header">
    <div class="workspace-card">
      <div class="workspace-card__copy">
        <p class="eyebrow">Waddle Chat</p>
        <h2>Workspace</h2>
        <p class="muted">Signed in as {session?.username ?? "guest"}</p>
      </div>
    </div>
  </header>

  <section class="section">
    <div class="section__header section__header--inline">
      <div>
        <p class="eyebrow">Communities</p>
        <p class="muted">Choose a workspace</p>
      </div>
      <Dialog.Root bind:open={showCreateWaddle} modal>
        <Dialog.Trigger>New</Dialog.Trigger>
        <Dialog.Backdrop class="app-dialog-backdrop" />
        <Dialog.Positioner class="app-dialog-positioner">
          <Dialog.Content class="app-dialog-content">
            <div class="app-dialog-header">
              <div class="stack stack--tight">
                <Dialog.Title>Create community</Dialog.Title>
                <p class="muted">Start a new workspace for rooms and members.</p>
              </div>
              <Dialog.CloseTrigger>Close</Dialog.CloseTrigger>
            </div>

            <form class="form form--compact app-dialog-body" on:submit|preventDefault={() => onCreate()}>
              <Field.Root class="form-block">
                <Field.Label>Name</Field.Label>
                <Field.Input bind:value={createWaddleForm.name} type="text" maxlength="80" required />
              </Field.Root>

              <Field.Root class="form-block">
                <Field.Label>Description</Field.Label>
                <Field.Textarea bind:value={createWaddleForm.description} rows="3" maxlength="240" />
              </Field.Root>

              <Checkbox.Root bind:checked={createWaddleForm.is_public}>
                <Checkbox.HiddenInput />
                <Checkbox.Control>
                  <Checkbox.Indicator>✓</Checkbox.Indicator>
                </Checkbox.Control>
                <Checkbox.Label>Public community</Checkbox.Label>
              </Checkbox.Root>

              <div class="actions app-dialog-actions">
                <Dialog.CloseTrigger>Cancel</Dialog.CloseTrigger>
                <button type="submit" disabled={isSubmitting}>Create community</button>
              </div>
            </form>
          </Dialog.Content>
        </Dialog.Positioner>
      </Dialog.Root>
    </div>

    <nav aria-label="Communities" class="community-list">
      {#if sortedWaddles.length}
        {#each sortedWaddles as waddle (waddle.id)}
          <button
            type="button"
            class="nav-item"
            aria-current={waddle.id === activeWaddleId ? "true" : undefined}
            on:click={() => onSelectWaddle(waddle.id)}
          >
            <span class="nav-item__title">{waddle.name}</span>
            <span class="nav-item__meta">{waddle.role ?? "member"}</span>
          </button>
        {/each}
      {:else}
        <p class="empty-state">No communities yet.</p>
      {/if}
    </nav>
  </section>

  <section class="section">
    <div class="section__header">
      <p class="eyebrow">Rooms</p>
      <p class="muted">
        {#if currentWaddle}
          {currentWaddle.name}
        {:else}
          Select a community first
        {/if}
      </p>
    </div>

    {#if !currentWaddle}
      <p class="empty-state">Pick a community to browse channels.</p>
    {:else if isLoadingStructure}
      <p class="empty-state">Loading rooms…</p>
    {:else if sortedChannels.length}
      <nav aria-label="Channels" class="channel-list">
        {#each sortedChannels as channel (channel.id)}
          <button
            type="button"
            class="nav-item nav-item--channel"
            aria-current={channel.id === activeChannelId ? "true" : undefined}
            on:click={() => onSelectChannel(channel.id)}
          >
            <span class="nav-item__title">#{channel.name}</span>
            <span class="nav-item__meta">
              {channel.description || (canManageChannels ? "Open room settings on the right" : "Conversation")}
            </span>
          </button>
        {/each}
      </nav>
    {:else}
      <p class="empty-state">No rooms yet.</p>
    {/if}
  </section>
</aside>
