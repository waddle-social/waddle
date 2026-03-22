<script lang="ts">
  import { Collapsible } from "@ark-ui/svelte/collapsible";
  import { Dialog } from "@ark-ui/svelte/dialog";
  import { Field } from "@ark-ui/svelte/field";
  import type { ChannelCreateFormData, ChannelEditFormData } from "../../lib/chat-ui";
  import type { ChannelSummary } from "../../lib/waddle-api";

  export let sortedChannels: ChannelSummary[] = [];
  export let currentChannel: ChannelSummary | null = null;
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
  export let canManageChannels = false;
  export let isLoadingStructure = false;
  export let isSubmitting = false;
  export let onCreateChannel: () => void | Promise<void>;
  export let onUpdateChannel: () => void | Promise<void>;
  export let onDeleteChannel: () => void | Promise<void>;
</script>

<section class="section">
  <header class="section__header">
    <p class="eyebrow">Room tools</p>
    <p class="muted">
      {#if isLoadingStructure}
        Loading room data…
      {:else}
        {sortedChannels.length} rooms in this community
      {/if}
    </p>
  </header>

  <div class="info-card">
    <span class="info-card__label">Current room</span>
    <strong>{currentChannel ? `#${currentChannel.name}` : "No room selected"}</strong>
    <span class="info-card__meta">
      {currentChannel?.description || "Select a room from the left rail to edit it here."}
    </span>
  </div>

  <Dialog.Root bind:open={showCreateChannel} modal>
    <div class="actions">
      <Dialog.Trigger disabled={!canManageChannels}>Create room</Dialog.Trigger>
    </div>

    <Dialog.Backdrop class="app-dialog-backdrop" />
    <Dialog.Positioner class="app-dialog-positioner">
      <Dialog.Content class="app-dialog-content">
        <div class="app-dialog-header">
          <div class="stack stack--tight">
            <Dialog.Title>Create room</Dialog.Title>
            <p class="muted">Add a new channel to {sortedChannels.length ? "this workspace" : "get started"}.</p>
          </div>
          <Dialog.CloseTrigger>Close</Dialog.CloseTrigger>
        </div>

        <form class="form form--compact app-dialog-body" on:submit|preventDefault={() => onCreateChannel()}>
          <Field.Root class="form-block">
            <Field.Label>Name</Field.Label>
            <Field.Input bind:value={createChannelForm.name} type="text" maxlength="80" required />
          </Field.Root>

          <Field.Root class="form-block">
            <Field.Label>Description</Field.Label>
            <Field.Textarea bind:value={createChannelForm.description} rows="2" maxlength="240" />
          </Field.Root>

          <Field.Root class="form-block">
            <Field.Label>Type</Field.Label>
            <Field.Select bind:value={createChannelForm.channel_type}>
              <option value="text">text</option>
            </Field.Select>
          </Field.Root>

          <Field.Root class="form-block">
            <Field.Label>Position</Field.Label>
            <Field.Input bind:value={createChannelForm.position} type="number" min="0" />
          </Field.Root>

          <div class="actions app-dialog-actions">
            <Dialog.CloseTrigger>Cancel</Dialog.CloseTrigger>
            <button type="submit" disabled={isSubmitting}>Create room</button>
          </div>
        </form>
      </Dialog.Content>
    </Dialog.Positioner>
  </Dialog.Root>

  {#if currentChannel}
    <Collapsible.Root bind:open={showEditChannel} class="section">
      <div class="actions">
        <Collapsible.Trigger disabled={!canManageChannels}>
          {showEditChannel ? "Hide room settings" : "Edit current room"}
        </Collapsible.Trigger>
      </div>

      <Collapsible.Content>
        <form class="form form--compact" on:submit|preventDefault={() => onUpdateChannel()}>
          <Field.Root class="form-block">
            <Field.Label>Name</Field.Label>
            <Field.Input bind:value={editChannelForm.name} type="text" maxlength="80" required />
          </Field.Root>

          <Field.Root class="form-block">
            <Field.Label>Description</Field.Label>
            <Field.Textarea bind:value={editChannelForm.description} rows="2" maxlength="240" />
          </Field.Root>

          <Field.Root class="form-block">
            <Field.Label>Position</Field.Label>
            <Field.Input bind:value={editChannelForm.position} type="number" min="0" />
          </Field.Root>

          <div class="actions">
            <button type="submit" disabled={isSubmitting}>Save room</button>
            <button
              type="button"
              disabled={isSubmitting || currentChannel.is_default}
              on:click={() => onDeleteChannel()}
            >
              Delete room
            </button>
          </div>
        </form>
      </Collapsible.Content>
    </Collapsible.Root>
  {/if}
</section>
