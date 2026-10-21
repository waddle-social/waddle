<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { Bot } from "lucide-vue-next";
import ConfirmDialog from "@/components/ui/ConfirmDialog.vue";
import {
  BOT_NAME_PATTERN,
  createBot,
  deleteBot,
  listBots,
  mintBotToken,
  revokeBotToken,
  type BotSummary,
  type BotTokenSummary,
  type BotsRequestContext,
  type BotsResult,
} from "@/lib/bots-api";

const props = defineProps<{
  serverBaseUrl: string;
  sessionId: string | null;
}>();

type FeedbackTone = "muted" | "success" | "error";

interface Feedback {
  tone: FeedbackTone;
  message: string;
}

interface MintDraft {
  label: string;
  expiresInDays: number;
}

const fieldLabelClass = "type-section-label text-muted-foreground/75";
const fieldClass = "chat-field-control type-field disabled:opacity-50";
const primaryButtonClass = "chat-action-button chat-action-button--primary type-control disabled:opacity-40";
const secondaryButtonClass = "chat-action-button chat-action-button--secondary type-control disabled:opacity-40";
const destructiveButtonClass = "chat-action-button chat-action-button--destructive type-control disabled:opacity-40";

const EXPIRY_OPTIONS = [30, 90, 365] as const;
const DEFAULT_EXPIRY_DAYS = 90;

const loadState = ref<"loading" | "ready" | "error">("loading");
const loadFeedback = ref<Feedback | null>(null);
const bots = ref<BotSummary[]>([]);
const limit = ref(0);
const tokenLimit = ref(0);
let loadAttempt = 0;

const createName = ref("");
const createDisplayName = ref("");
const createBusy = ref(false);
const createFeedback = ref<Feedback>({
  tone: "muted",
  message: "Bots get their own XMPP address and act only where you grant them access.",
});

const mintDrafts = reactive<Record<string, MintDraft>>({});
const botFeedback = reactive<Record<string, Feedback>>({});
const busyBot = ref<string | null>(null);

// The minted secret lives only in this component's state. It is never
// written to stores, storage, toasts or telemetry, and is cleared on
// dismiss, on context change and on unmount.
const mintedSecret = ref<{ botJid: string; label: string; secret: string } | null>(null);
const copyFeedback = ref<Feedback | null>(null);

const pendingRevoke = ref<{ bot: BotSummary; token: BotTokenSummary } | null>(null);
const pendingDelete = ref<BotSummary | null>(null);
const confirmBusy = ref(false);

const showRevoke = computed({
  get: () => pendingRevoke.value !== null,
  set: (open: boolean) => {
    if (!open) pendingRevoke.value = null;
  },
});

const showDelete = computed({
  get: () => pendingDelete.value !== null,
  set: (open: boolean) => {
    if (!open) pendingDelete.value = null;
  },
});

const nameError = computed(() => {
  const name = createName.value;
  if (!name || BOT_NAME_PATTERN.test(name)) return null;
  return "Use 1–32 lowercase letters, numbers, - or _, starting and ending with a letter or number.";
});

const atBotLimit = computed(() => loadState.value === "ready" && bots.value.length >= limit.value);

const countCaption = computed(() => {
  if (loadState.value !== "ready") return "";
  if (limit.value === 0) return "Bot creation is turned off on this server.";
  return `${bots.value.length} of ${limit.value} bots`;
});

const canCreate = computed(() =>
  loadState.value === "ready"
  && !createBusy.value
  && !atBotLimit.value
  && createName.value.length > 0
  && nameError.value === null,
);

function context(): BotsRequestContext {
  return { serverBaseUrl: props.serverBaseUrl, sessionId: props.sessionId };
}

function feedbackClass(tone: FeedbackTone): string {
  switch (tone) {
    case "success":
      return "text-emerald-700 dark:text-emerald-300";
    case "error":
      return "text-destructive-text";
    default:
      return "text-muted-foreground";
  }
}

function errorMessage(result: Exclude<BotsResult<unknown>, { kind: "ok" }>): string {
  switch (result.kind) {
    case "unauthorized":
      return "Your session has ended. Sign in again to manage bots.";
    case "forbidden":
      return result.code === "owner_ineligible"
        ? "Accounts whose username contains + can't own bots."
        : "You need server membership to create bots.";
    case "conflict":
      switch (result.code) {
        case "bot_limit_reached":
          return `You've reached the limit of ${limit.value} bots.`;
        case "bot_exists":
          return "You already have a bot with that name.";
        case "bot_disabled":
          return "This bot is disabled. New tokens can be minted once its owner is restored.";
        case "bot_token_limit_reached":
          return tokenLimitMessage();
        case "bot_sole_room_owner":
          return "This bot is the only owner of a channel. Make someone else an owner of that channel, then delete the bot.";
      }
      break;
    case "invalid":
      switch (result.code) {
        case "invalid_name":
          return "That bot name isn't valid.";
        case "invalid_token_request":
          return "Token labels need 1–64 characters.";
        default:
          return "The server couldn't accept that request.";
      }
    case "not-found":
      return "That bot or token no longer exists.";
    case "failed":
      return "Couldn't reach the server. Try again.";
  }
  return "Something went wrong.";
}

function tokenLimitMessage(): string {
  return `This bot has the maximum of ${tokenLimit.value} active tokens. Revoke one to mint another.`;
}

function isActiveToken(token: BotTokenSummary, now: number): boolean {
  return token.revokedAt === null && Date.parse(token.expiresAt) > now;
}

function activeTokenCount(bot: BotSummary): number {
  const now = Date.now();
  return bot.tokens.filter((token) => isActiveToken(token, now)).length;
}

function tokenState(token: BotTokenSummary): string {
  if (token.revokedAt !== null) return `Revoked ${formatDate(token.revokedAt)}`;
  if (Date.parse(token.expiresAt) <= Date.now()) return `Expired ${formatDate(token.expiresAt)}`;
  return `Expires ${formatDate(token.expiresAt)}`;
}

function disabledLabel(bot: BotSummary): string {
  return bot.disabledReason === "owner_not_member"
    ? "Disabled: owner not a member"
    : "Disabled: owner missing";
}

function formatDate(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleDateString(undefined, { dateStyle: "medium" });
}

function syncMintDrafts(list: BotSummary[]) {
  const jids = new Set(list.map((bot) => bot.jid));
  for (const jid of Object.keys(mintDrafts)) {
    if (!jids.has(jid)) delete mintDrafts[jid];
  }
  for (const jid of jids) {
    mintDrafts[jid] ??= { label: "", expiresInDays: DEFAULT_EXPIRY_DAYS };
  }
}

function mintBlocked(bot: BotSummary): boolean {
  return bot.disabledAt !== null || activeTokenCount(bot) >= tokenLimit.value;
}

function canMint(bot: BotSummary): boolean {
  return !mintBlocked(bot)
    && busyBot.value === null
    && (mintDrafts[bot.jid]?.label.trim().length ?? 0) > 0;
}

async function load() {
  const attempt = ++loadAttempt;
  const result = await listBots(context());
  if (attempt !== loadAttempt) return;
  if (result.kind === "ok") {
    syncMintDrafts(result.value.bots);
    bots.value = result.value.bots;
    limit.value = result.value.limit;
    tokenLimit.value = result.value.tokenLimit;
    loadState.value = "ready";
    loadFeedback.value = null;
    return;
  }
  if (loadState.value !== "ready") loadState.value = "error";
  loadFeedback.value = { tone: "error", message: errorMessage(result) };
}

async function submitCreate() {
  if (!canCreate.value) return;
  createBusy.value = true;
  try {
    const result = await createBot(context(), {
      name: createName.value,
      displayName: createDisplayName.value,
    });
    if (result.kind === "ok") {
      createName.value = "";
      createDisplayName.value = "";
      createFeedback.value = { tone: "success", message: `Created ${result.value.jid}.` };
      await load();
      return;
    }
    createFeedback.value = { tone: "error", message: errorMessage(result) };
    if (result.kind === "conflict") await load();
  } finally {
    createBusy.value = false;
  }
}

async function submitMint(bot: BotSummary) {
  const draft = mintDrafts[bot.jid];
  if (!draft || !canMint(bot)) return;
  busyBot.value = bot.jid;
  try {
    const result = await mintBotToken(context(), bot.jid, {
      label: draft.label.trim(),
      expiresInDays: draft.expiresInDays,
    });
    if (result.kind === "ok") {
      mintedSecret.value = {
        botJid: bot.jid,
        label: result.value.token.label,
        secret: result.value.secret,
      };
      copyFeedback.value = null;
      draft.label = "";
      draft.expiresInDays = DEFAULT_EXPIRY_DAYS;
      delete botFeedback[bot.jid];
      await load();
      return;
    }
    botFeedback[bot.jid] = { tone: "error", message: errorMessage(result) };
    if (result.kind === "conflict" || result.kind === "not-found") await load();
  } finally {
    busyBot.value = null;
  }
}

async function confirmRevoke() {
  const pending = pendingRevoke.value;
  if (!pending) return;
  confirmBusy.value = true;
  try {
    const result = await revokeBotToken(context(), pending.bot.jid, pending.token.id);
    botFeedback[pending.bot.jid] = result.kind === "ok"
      ? { tone: "success", message: `Revoked ${pending.token.label}.` }
      : { tone: "error", message: errorMessage(result) };
    pendingRevoke.value = null;
    await load();
  } finally {
    confirmBusy.value = false;
  }
}

async function confirmDelete() {
  const bot = pendingDelete.value;
  if (!bot) return;
  confirmBusy.value = true;
  try {
    const result = await deleteBot(context(), bot.jid);
    if (result.kind === "ok" || result.kind === "not-found") {
      if (mintedSecret.value?.botJid === bot.jid) dismissSecret();
      delete botFeedback[bot.jid];
      createFeedback.value = { tone: "success", message: `Deleted ${bot.jid}.` };
    } else {
      botFeedback[bot.jid] = { tone: "error", message: errorMessage(result) };
    }
    pendingDelete.value = null;
    await load();
  } finally {
    confirmBusy.value = false;
  }
}

async function copySecret() {
  const minted = mintedSecret.value;
  if (!minted) return;
  try {
    if (typeof navigator === "undefined" || !navigator.clipboard?.writeText) {
      throw new Error("clipboard unavailable");
    }
    await navigator.clipboard.writeText(minted.secret);
    copyFeedback.value = { tone: "success", message: "Copied." };
  } catch {
    copyFeedback.value = { tone: "error", message: "Couldn't copy. Select the token and copy it manually." };
  }
}

function selectOnFocus(event: FocusEvent) {
  if (event.target instanceof HTMLInputElement) event.target.select();
}

function dismissSecret() {
  mintedSecret.value = null;
  copyFeedback.value = null;
}

onMounted(() => {
  void load();
});

watch(
  () => [props.serverBaseUrl, props.sessionId],
  () => {
    dismissSecret();
    loadState.value = "loading";
    bots.value = [];
    void load();
  },
);

onBeforeUnmount(() => {
  loadAttempt += 1;
  dismissSecret();
});
</script>

<template>
  <section class="chat-section-card glass-panel" aria-labelledby="bots-settings-title">
    <div class="flex items-start gap-3 pb-4">
      <div class="flex h-8 w-8 items-center justify-center rounded-lg bg-primary/10 text-primary">
        <Bot class="h-4 w-4" />
      </div>
      <div class="chat-settings-copy max-w-2xl">
        <h2 id="bots-settings-title" class="type-pane-title">Bots</h2>
        <p class="type-caption text-muted-foreground">
          Create bot accounts that act for you with their own tokens. A bot can never do more than you can.
        </p>
        <p v-if="countCaption" class="type-caption text-muted-foreground">{{ countCaption }}</p>
      </div>
    </div>

    <div class="divide-y divide-border/70">
      <form class="chat-settings-form pb-4" @submit.prevent="submitCreate">
        <div class="chat-settings-copy">
          <h3 class="type-pane-title">New bot</h3>
          <p class="type-caption text-muted-foreground">
            The server picks the bot's address from your username and the name.
          </p>
        </div>

        <div class="grid gap-3">
          <div class="grid gap-3 sm:grid-cols-2">
            <label class="grid gap-1.5">
              <span :class="fieldLabelClass">Name</span>
              <input
                v-model="createName"
                :class="fieldClass"
                :aria-invalid="nameError !== null"
                autocomplete="off"
                maxlength="32"
                placeholder="standup"
                spellcheck="false"
                type="text"
              />
              <p
                class="type-caption"
                :class="nameError ? 'text-destructive-text' : 'text-muted-foreground'"
              >
                {{ nameError ?? "Lowercase letters, numbers, - and _." }}
              </p>
            </label>

            <label class="grid gap-1.5">
              <span :class="fieldLabelClass">Display name</span>
              <input
                v-model="createDisplayName"
                :class="fieldClass"
                maxlength="64"
                placeholder="Standup Bot"
                type="text"
              />
              <p class="type-caption text-muted-foreground">Optional.</p>
            </label>
          </div>

          <div class="flex flex-wrap items-center gap-2">
            <button type="submit" :class="primaryButtonClass" :disabled="!canCreate">
              {{ createBusy ? "Creating…" : "Create bot" }}
            </button>
            <p class="type-caption" :class="feedbackClass(createFeedback.tone)" aria-live="polite">
              {{ createFeedback.message }}
            </p>
          </div>
        </div>
      </form>

      <div
        v-if="mintedSecret"
        class="grid gap-2 py-4"
        role="region"
        aria-label="New bot token"
      >
        <p class="type-section-label text-muted-foreground/70">
          New token “{{ mintedSecret.label }}” for {{ mintedSecret.botJid }}
        </p>
        <div class="flex flex-wrap items-center gap-2">
          <input
            :class="fieldClass"
            class="min-w-0 flex-1 font-mono"
            :value="mintedSecret.secret"
            aria-label="Bot token"
            readonly
            spellcheck="false"
            type="text"
            @focus="selectOnFocus"
          />
          <button type="button" :class="primaryButtonClass" @click="copySecret">Copy</button>
          <button type="button" :class="secondaryButtonClass" @click="dismissSecret">Done</button>
        </div>
        <p class="type-caption text-muted-foreground">
          Copy this token now. You won't be able to see it again.
        </p>
        <p
          v-if="copyFeedback"
          class="type-caption"
          :class="feedbackClass(copyFeedback.tone)"
          aria-live="polite"
        >
          {{ copyFeedback.message }}
        </p>
      </div>

      <div class="pt-4">
        <p
          v-if="loadFeedback"
          class="type-caption pb-3"
          :class="feedbackClass(loadFeedback.tone)"
          aria-live="polite"
        >
          {{ loadFeedback.message }}
        </p>

        <p v-if="loadState === 'loading'" class="type-caption text-muted-foreground" role="status">
          Loading bots…
        </p>
        <div v-else-if="loadState === 'error'" class="flex items-center gap-2">
          <button type="button" :class="secondaryButtonClass" @click="load">Try again</button>
        </div>
        <p v-else-if="bots.length === 0" class="type-caption text-muted-foreground">
          You don't have any bots yet.
        </p>

        <ul v-else class="grid gap-4">
          <li
            v-for="bot in bots"
            :key="bot.jid"
            class="grid gap-3 rounded-lg border border-border/70 p-3"
          >
            <div class="flex flex-wrap items-start justify-between gap-2">
              <div class="min-w-0">
                <p class="type-pane-title truncate">{{ bot.displayName ?? bot.name }}</p>
                <p class="type-caption break-all text-muted-foreground">{{ bot.jid }}</p>
              </div>
              <div class="flex flex-wrap items-center gap-2">
                <span
                  v-if="bot.disabledAt"
                  class="type-caption rounded-full border border-destructive/30 bg-destructive/10 px-2 py-0.5 text-destructive-text"
                >
                  {{ disabledLabel(bot) }}
                </span>
                <button
                  type="button"
                  :class="destructiveButtonClass"
                  :disabled="busyBot !== null"
                  @click="pendingDelete = bot"
                >
                  Delete bot
                </button>
              </div>
            </div>

            <p v-if="bot.disabledAt" class="type-caption text-muted-foreground">
              Its tokens were revoked when it was disabled. Once the owner is restored, mint new tokens.
            </p>

            <div class="grid gap-2">
              <p :class="fieldLabelClass">Tokens</p>
              <p v-if="bot.tokens.length === 0" class="type-caption text-muted-foreground">No tokens yet.</p>
              <ul v-else class="grid gap-2">
                <li
                  v-for="token in bot.tokens"
                  :key="token.id"
                  class="flex flex-wrap items-center justify-between gap-2"
                >
                  <div class="min-w-0">
                    <p class="type-field truncate">{{ token.label }}</p>
                    <p class="type-caption text-muted-foreground">
                      Created {{ formatDate(token.createdAt) }} ·
                      {{ token.lastUsedAt ? `Last used ${formatDate(token.lastUsedAt)}` : "Never used" }} ·
                      {{ tokenState(token) }}
                    </p>
                  </div>
                  <button
                    v-if="token.revokedAt === null"
                    type="button"
                    :class="secondaryButtonClass"
                    :disabled="busyBot !== null"
                    @click="pendingRevoke = { bot, token }"
                  >
                    Revoke
                  </button>
                </li>
              </ul>
            </div>

            <form v-if="mintDrafts[bot.jid]" class="grid gap-2" @submit.prevent="submitMint(bot)">
              <div class="grid gap-3 sm:grid-cols-[1fr_auto]">
                <label class="grid gap-1.5">
                  <span :class="fieldLabelClass">Token label</span>
                  <input
                    v-model="mintDrafts[bot.jid].label"
                    :class="fieldClass"
                    :disabled="mintBlocked(bot)"
                    maxlength="64"
                    placeholder="laptop"
                    type="text"
                  />
                </label>
                <label class="grid gap-1.5">
                  <span :class="fieldLabelClass">Expires in</span>
                  <select
                    v-model.number="mintDrafts[bot.jid].expiresInDays"
                    :class="fieldClass"
                    :disabled="mintBlocked(bot)"
                  >
                    <option v-for="days in EXPIRY_OPTIONS" :key="days" :value="days">{{ days }} days</option>
                  </select>
                </label>
              </div>
              <div class="flex flex-wrap items-center gap-2">
                <button type="submit" :class="secondaryButtonClass" :disabled="!canMint(bot)">
                  {{ busyBot === bot.jid ? "Minting…" : "Mint token" }}
                </button>
                <p
                  v-if="bot.disabledAt === null && activeTokenCount(bot) >= tokenLimit"
                  class="type-caption text-muted-foreground"
                >
                  {{ tokenLimitMessage() }}
                </p>
                <p
                  v-if="botFeedback[bot.jid]"
                  class="type-caption"
                  :class="feedbackClass(botFeedback[bot.jid].tone)"
                  aria-live="polite"
                >
                  {{ botFeedback[bot.jid].message }}
                </p>
              </div>
            </form>
          </li>
        </ul>
      </div>
    </div>

    <ConfirmDialog
      v-model:open="showRevoke"
      title="Revoke token?"
      :message="pendingRevoke
        ? `Anything using “${pendingRevoke.token.label}” loses access to ${pendingRevoke.bot.jid} immediately.`
        : ''"
      confirm-label="Revoke"
      destructive
      :loading="confirmBusy"
      @confirm="confirmRevoke"
    />
    <ConfirmDialog
      v-model:open="showDelete"
      title="Delete bot?"
      :message="pendingDelete
        ? `This deletes ${pendingDelete.jid}, removes it from every channel and space, and revokes all of its tokens.`
        : ''"
      confirm-label="Delete bot"
      destructive
      :loading="confirmBusy"
      @confirm="confirmDelete"
    />
  </section>
</template>
