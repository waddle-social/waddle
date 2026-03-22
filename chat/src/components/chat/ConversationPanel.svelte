<script lang="ts">
  import { Field } from "@ark-ui/svelte/field";
  import type { WaddleSession } from "../../lib/server-auth";
  import type { ChannelSummary, WaddleSummary } from "../../lib/waddle-api";
  import type { TimelineMessage } from "../../lib/chat-ui";
  import type { XmppStatusSnapshot } from "../../lib/xmpp-client";

  export let currentWaddle: WaddleSummary | null = null;
  export let currentChannel: ChannelSummary | null = null;
  export let currentRoomJid: string | null = null;
  export let session: WaddleSession | null = null;
  export let xmppStatus: XmppStatusSnapshot;
  export let actionError = "";
  export let messages: TimelineMessage[] = [];
  export let isLoadingMessages = false;
  export let isBootstrapping = false;
  export let draft = "";
  export let isSending = false;
  export let timelineViewport: HTMLDivElement | null = null;
  export let formatStamp: (value: string) => string;
  export let onLogout: () => void | Promise<void>;
  export let onSend: () => void | Promise<void>;
</script>

<main class="panel conversation-panel">
  <header class="conversation-header">
    <div class="stack stack--tight">
      <div class="conversation-heading">
        <div>
          <p class="eyebrow">{currentWaddle?.name ?? "Waddle Chat"}</p>
          <h1>{currentChannel ? `#${currentChannel.name}` : "Pick a room"}</h1>
        </div>
        <div class="presence-chip" data-state={xmppStatus.state}>{xmppStatus.detail}</div>
      </div>

      {#if currentChannel}
        <p class="conversation-copy">
          {currentChannel.description || "Live room connected over Waddle server."}
        </p>
      {:else}
        <p class="conversation-copy">Choose a room from the left to load history and join live chat.</p>
      {/if}
    </div>

    <div class="conversation-toolbar">
      <div class="identity-card">
        <span class="identity-card__name">{session?.username}</span>
        <span class="identity-card__meta">{session?.jid}</span>
        {#if currentRoomJid}
          <span class="identity-card__meta">{currentRoomJid}</span>
        {/if}
      </div>
      <button type="button" on:click={() => onLogout()}>Log out</button>
    </div>
  </header>

  {#if actionError}
    <p class="error">{actionError}</p>
  {/if}

  <section class="timeline-frame">
    <div class="timeline" bind:this={timelineViewport}>
      {#if isLoadingMessages || isBootstrapping}
        <p class="empty-state">Loading messages…</p>
      {:else if messages.length}
        <div class="message-list">
          {#each messages as message (message.id)}
            <article class:message--self={message.isSelf} class="message">
              <div class="message__meta">
                <strong>{message.author}</strong>
                <span>{formatStamp(message.createdAt)}</span>
                {#if message.isSelf}
                  <span>you</span>
                {/if}
              </div>
              <p>{message.body}</p>
            </article>
          {/each}
        </div>
      {:else if currentChannel}
        <p class="empty-state">No messages yet. Start the conversation.</p>
      {:else}
        <p class="empty-state">Choose a room to load history.</p>
      {/if}
    </div>
  </section>

  <form class="composer composer--chat" on:submit|preventDefault={() => onSend()}>
    <Field.Root class="form-block composer__field">
      <Field.Label>Message</Field.Label>
      <Field.Textarea
        bind:value={draft}
        rows="3"
        placeholder={currentChannel ? `Message #${currentChannel.name}` : "Pick a room first"}
        disabled={!currentChannel || xmppStatus.state !== "online"}
        on:keydown={(event) => {
          if (event.key === "Enter" && !event.shiftKey) {
            event.preventDefault();
            void onSend();
          }
        }}
      />
    </Field.Root>

    <div class="composer__actions">
      <button
        type="submit"
        disabled={!draft.trim() || !currentChannel || isSending || xmppStatus.state !== "online"}
      >
        {isSending ? "Sending…" : "Send"}
      </button>
    </div>
  </form>
</main>
