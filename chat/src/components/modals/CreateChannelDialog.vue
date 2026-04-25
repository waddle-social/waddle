<script setup lang="ts">
import { computed } from "vue";
import { FolderPlus, Hash, LayoutGrid, MessagesSquare, Sparkles, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
import type {
  CreateFormData,
  CreateIntent,
  CreateMucFormData,
  CreateSpaceFormData,
  CreateSpaceMucFormData,
  CreateSpaceWithMucFormData,
  MucSubtype,
} from "@/lib/chat-ui";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{
  form: CreateFormData;
  isSubmitting: boolean;
  /** Available spaces for the "MUC in existing Space" intent (node id → display name). */
  spaces?: { node: string; name: string }[];
}>();

const emit = defineEmits<{
  submit: [];
  "update:form": [form: CreateFormData];
}>();

// ── Intent helpers ────────────────────────────────────────────────

const INTENTS: { value: CreateIntent; label: string; caption: string; icon: unknown }[] = [
  {
    value: "space",
    label: "Space",
    caption: "A new community space. Add channels later.",
    icon: LayoutGrid,
  },
  {
    value: "muc",
    label: "Channel",
    caption: "A standalone real-time channel.",
    icon: Hash,
  },
  {
    value: "space-muc",
    label: "Channel in Space",
    caption: "Add a channel to an existing space.",
    icon: FolderPlus,
  },
  {
    value: "space-with-muc",
    label: "Space + Channel",
    caption: "New space and its first channel together.",
    icon: Sparkles,
  },
];

function selectIntent(intent: CreateIntent) {
  switch (intent) {
    case "space":
      emit("update:form", { intent: "space", name: "", description: "" } satisfies CreateSpaceFormData);
      break;
    case "muc":
      emit("update:form", { intent: "muc", name: "", description: "", muc_type: "text" } satisfies CreateMucFormData);
      break;
    case "space-muc":
      emit("update:form", { intent: "space-muc", space_jid: "", name: "", description: "", muc_type: "text" } satisfies CreateSpaceMucFormData);
      break;
    case "space-with-muc":
      emit("update:form", { intent: "space-with-muc", space_name: "", space_description: "", muc_name: "", muc_description: "", muc_type: "text" } satisfies CreateSpaceWithMucFormData);
      break;
  }
}

function setMucType(subtype: MucSubtype) {
  if (props.form.intent === "muc") {
    emit("update:form", { ...props.form, muc_type: subtype });
  } else if (props.form.intent === "space-muc") {
    emit("update:form", { ...props.form, muc_type: subtype });
  } else if (props.form.intent === "space-with-muc") {
    emit("update:form", { ...props.form, muc_type: subtype });
  }
}

const hasMucType = computed(() =>
  props.form.intent === "muc" || props.form.intent === "space-muc" || props.form.intent === "space-with-muc",
);

const currentMucType = computed<MucSubtype | null>(() => {
  if (props.form.intent === "muc") return props.form.muc_type;
  if (props.form.intent === "space-muc") return props.form.muc_type;
  if (props.form.intent === "space-with-muc") return props.form.muc_type;
  return null;
});

const isSubmitDisabled = computed(() => {
  if (props.isSubmitting) return true;
  const f = props.form;
  if (f.intent === "space") return !f.name.trim();
  if (f.intent === "muc") return !f.name.trim();
  if (f.intent === "space-muc") return !f.space_jid.trim() || !f.name.trim();
  if (f.intent === "space-with-muc") return !f.space_name.trim() || !f.muc_name.trim();
  return true;
});

const dialogTitle = computed(() => {
  switch (props.form.intent) {
    case "space": return "New Space";
    case "muc": return "New Channel";
    case "space-muc": return "New Channel in Space";
    case "space-with-muc": return "New Space + Channel";
  }
});
</script>

<template>
  <AppDialog v-model:open="open">
    <div class="chat-dialog-header">
      <h2 class="type-dialog-title">{{ dialogTitle }}</h2>
      <button
        class="chat-icon-button hover:bg-muted"
        type="button"
        aria-label="Close create dialog"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="chat-dialog-stack chat-dialog-body">
      <!-- Intent selector -->
      <div class="chat-field-stack">
        <label class="type-section-label text-muted-foreground">What would you like to create?</label>
        <div class="grid grid-cols-2 gap-2">
          <button
            v-for="opt in INTENTS"
            :key="opt.value"
            type="button"
            class="flex flex-col gap-1 rounded-lg border px-3 py-3 text-left transition-all duration-200"
            :aria-pressed="form.intent === opt.value"
            :class="form.intent === opt.value
              ? 'border-primary/30 bg-primary/8 shadow-[0_0_12px_var(--glow)]'
              : 'border-border bg-muted/40 hover:bg-muted'"
            @click="selectIntent(opt.value)"
          >
            <div class="type-control flex items-center gap-2">
              <component :is="opt.icon" class="w-4 h-4 text-primary/80" />
              {{ opt.label }}
            </div>
            <p class="type-caption text-muted-foreground">{{ opt.caption }}</p>
          </button>
        </div>
      </div>

      <!-- ── Space fields ── -->
      <template v-if="form.intent === 'space'">
        <div class="chat-field-stack">
          <label for="create-space-name" class="type-section-label text-muted-foreground">Name</label>
          <input
            id="create-space-name"
            :value="form.name"
            class="chat-field-control type-field"
            placeholder="My Community"
            @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
          />
        </div>
        <div class="chat-field-stack">
          <label for="create-space-desc" class="type-section-label text-muted-foreground">Description</label>
          <textarea
            id="create-space-desc"
            :value="form.description"
            class="chat-field-control chat-textarea-control type-field"
            placeholder="What is this space for?"
            @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
          />
        </div>
      </template>

      <!-- ── MUC fields ── -->
      <template v-else-if="form.intent === 'muc'">
        <div class="chat-field-stack">
          <label for="create-muc-name" class="type-section-label text-muted-foreground">Name</label>
          <input
            id="create-muc-name"
            :value="form.name"
            class="chat-field-control type-field"
            placeholder="general"
            @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
          />
        </div>
        <div class="chat-field-stack">
          <label for="create-muc-desc" class="type-section-label text-muted-foreground">Description</label>
          <textarea
            id="create-muc-desc"
            :value="form.description"
            class="chat-field-control chat-textarea-control type-field"
            placeholder="What is this channel for?"
            @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
          />
        </div>
      </template>

      <!-- ── MUC in existing Space fields ── -->
      <template v-else-if="form.intent === 'space-muc'">
        <div class="chat-field-stack">
          <label for="create-spacemuc-space" class="type-section-label text-muted-foreground">Target Space</label>
          <template v-if="spaces && spaces.length > 0">
            <select
              id="create-spacemuc-space"
              :value="form.space_jid"
              class="chat-field-control type-field"
              @change="$emit('update:form', { ...form, space_jid: ($event.target as HTMLSelectElement).value })"
            >
              <option value="" disabled>Select a space…</option>
              <option v-for="s in spaces" :key="s.node" :value="s.node">{{ s.name }}</option>
            </select>
          </template>
          <template v-else>
            <input
              id="create-spacemuc-space"
              :value="form.space_jid"
              class="chat-field-control type-field"
              placeholder="team-space"
              @input="$emit('update:form', { ...form, space_jid: ($event.target as HTMLInputElement).value })"
            />
            <p class="type-caption text-muted-foreground">Enter an existing Spaces service node.</p>
          </template>
        </div>
        <div class="chat-field-stack">
          <label for="create-spacemuc-name" class="type-section-label text-muted-foreground">Channel Name</label>
          <input
            id="create-spacemuc-name"
            :value="form.name"
            class="chat-field-control type-field"
            placeholder="general"
            @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
          />
        </div>
        <div class="chat-field-stack">
          <label for="create-spacemuc-desc" class="type-section-label text-muted-foreground">Description</label>
          <textarea
            id="create-spacemuc-desc"
            :value="form.description"
            class="chat-field-control chat-textarea-control type-field"
            placeholder="What is this channel for?"
            @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
          />
        </div>
      </template>

      <!-- ── Space + MUC fields ── -->
      <template v-else-if="form.intent === 'space-with-muc'">
        <div class="chat-field-stack">
          <label for="create-swm-sname" class="type-section-label text-muted-foreground">Space Name</label>
          <input
            id="create-swm-sname"
            :value="form.space_name"
            class="chat-field-control type-field"
            placeholder="My Community"
            @input="$emit('update:form', { ...form, space_name: ($event.target as HTMLInputElement).value })"
          />
        </div>
        <div class="chat-field-stack">
          <label for="create-swm-sdesc" class="type-section-label text-muted-foreground">Space Description</label>
          <textarea
            id="create-swm-sdesc"
            :value="form.space_description"
            class="chat-field-control chat-textarea-control type-field"
            placeholder="What is this space for?"
            @input="$emit('update:form', { ...form, space_description: ($event.target as HTMLTextAreaElement).value })"
          />
        </div>
        <div class="chat-field-stack">
          <label for="create-swm-mname" class="type-section-label text-muted-foreground">Channel Name</label>
          <input
            id="create-swm-mname"
            :value="form.muc_name"
            class="chat-field-control type-field"
            placeholder="general"
            @input="$emit('update:form', { ...form, muc_name: ($event.target as HTMLInputElement).value })"
          />
        </div>
        <div class="chat-field-stack">
          <label for="create-swm-mdesc" class="type-section-label text-muted-foreground">Channel Description</label>
          <textarea
            id="create-swm-mdesc"
            :value="form.muc_description"
            class="chat-field-control chat-textarea-control type-field"
            placeholder="What is this channel for?"
            @input="$emit('update:form', { ...form, muc_description: ($event.target as HTMLTextAreaElement).value })"
          />
        </div>
      </template>

      <!-- ── MUC subtype picker (shown for all intents that create a MUC) ── -->
      <div v-if="hasMucType" class="chat-field-stack">
        <label class="type-section-label text-muted-foreground">Channel type</label>
        <div class="grid grid-cols-2 gap-2">
          <button
            type="button"
            class="flex flex-col gap-1 rounded-lg border px-3 py-3 text-left transition-all duration-200"
            :aria-pressed="currentMucType === 'text'"
            :class="currentMucType === 'text'
              ? 'border-primary/30 bg-primary/8 shadow-[0_0_12px_var(--glow)]'
              : 'border-border bg-muted/40 hover:bg-muted'"
            @click="setMucType('text')"
          >
            <div class="type-control flex items-center gap-2">
              <Hash class="w-4 h-4 text-primary/80" />
              Text
            </div>
            <p class="type-caption text-muted-foreground">Fast back-and-forth conversation.</p>
          </button>
          <button
            type="button"
            class="flex flex-col gap-1 rounded-lg border px-3 py-3 text-left transition-all duration-200"
            :aria-pressed="currentMucType === 'forum'"
            :class="currentMucType === 'forum'
              ? 'border-primary/30 bg-primary/8 shadow-[0_0_12px_var(--glow)]'
              : 'border-border bg-muted/40 hover:bg-muted'"
            @click="setMucType('forum')"
          >
            <div class="type-control flex items-center gap-2">
              <MessagesSquare class="w-4 h-4 text-primary/80" />
              Forum
            </div>
            <p class="type-caption text-muted-foreground">Title-first topics with threaded replies.</p>
          </button>
        </div>
      </div>
    </div>

    <div class="chat-dialog-footer">
      <button
        class="chat-action-button chat-action-button--secondary type-control"
        type="button"
        @click="open = false"
      >
        Cancel
      </button>
      <button
        class="chat-action-button chat-action-button--primary type-control disabled:opacity-40"
        type="button"
        :disabled="isSubmitDisabled"
        @click="emit('submit')"
      >
        {{ isSubmitting ? "Creating…" : "Create" }}
      </button>
    </div>
  </AppDialog>
</template>
