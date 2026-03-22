<script lang="ts">
  export let title: string;
  export let copy = "";
  export let actionLabel = "";
  export let onAction: (() => void | Promise<void>) | null = null;
</script>

<section class="centered">
  <div class="stack">
    <div class="stack">
      <p class="muted">Waddle Chat</p>
      <h1>{title}</h1>
      {#if copy}
        <p>{copy}</p>
      {/if}
    </div>

    {#if actionLabel && onAction}
      <div class="actions">
        <button type="button" on:click={() => onAction?.()}>{actionLabel}</button>
      </div>
    {/if}
  </div>
</section>
