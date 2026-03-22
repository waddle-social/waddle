<script lang="ts">
  import { Checkbox } from "@ark-ui/svelte/checkbox";
  import { Field } from "@ark-ui/svelte/field";
  import type { CommunityFormData } from "../../lib/chat-ui";
  import type { WaddleSummary } from "../../lib/waddle-api";

  export let currentWaddle: WaddleSummary | null = null;
  export let editWaddleForm: CommunityFormData = {
    name: "",
    description: "",
    is_public: true,
  };
  export let canManageCommunity = false;
  export let isSubmitting = false;
  export let onUpdateWaddle: () => void | Promise<void>;
  export let onDeleteWaddle: () => void | Promise<void>;
</script>

<section class="section">
  <header class="section__header">
    <p class="muted">Community settings</p>
  </header>

  {#if currentWaddle}
    <form class="form" on:submit|preventDefault={() => onUpdateWaddle()}>
      <Field.Root class="form-block">
        <Field.Label>Name</Field.Label>
        <Field.Input bind:value={editWaddleForm.name} type="text" maxlength="80" required disabled={!canManageCommunity} />
      </Field.Root>

      <Field.Root class="form-block">
        <Field.Label>Description</Field.Label>
        <Field.Textarea bind:value={editWaddleForm.description} rows="3" maxlength="240" disabled={!canManageCommunity} />
      </Field.Root>

      <Checkbox.Root bind:checked={editWaddleForm.is_public} disabled={!canManageCommunity}>
        <Checkbox.HiddenInput />
        <Checkbox.Control>
          [<Checkbox.Indicator>✓</Checkbox.Indicator>]
        </Checkbox.Control>
        <Checkbox.Label>Public community</Checkbox.Label>
      </Checkbox.Root>

      <div class="actions">
        <button type="submit" disabled={!canManageCommunity || isSubmitting}>Save</button>
        <button type="button" disabled={!canManageCommunity || isSubmitting} on:click={() => onDeleteWaddle()}>
          Delete
        </button>
      </div>
    </form>
  {:else}
    <p>Select a community first.</p>
  {/if}
</section>
