<script setup lang="ts">
import AppSelect from "@/components/ui/AppSelect.vue";
import { computed, type Component } from "vue";
import { FolderPlus, Hash, LayoutGrid, MessagesSquare, Sparkles, X } from "lucide-vue-next";
import { iconbtn, btn, option } from "styled-system/recipes";
import AppDialog from "@/components/ui/AppDialog.vue";
import { dialogParts } from "@/ui/part-classes";
import { formClasses as f } from "@/components/modals/form-classes";
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
  defaultSpaceNode?: string | null;
}>();

const emit = defineEmits<{
  submit: [];
  "update:form": [form: CreateFormData];
}>();

// ── Intent helpers ────────────────────────────────────────────────

const INTENTS: { value: CreateIntent; label: string; caption: string; icon: Component }[] = [
  {
    value: "space",
    label: "Space",
    caption: "A group of rooms. Add rooms later.",
    icon: LayoutGrid,
  },
  {
    value: "muc",
    label: "Room",
    caption: "Chat or forum, on its own.",
    icon: Hash,
  },
  {
    value: "space-muc",
    label: "Room in a space",
    caption: "Add a room to an existing space.",
    icon: FolderPlus,
  },
  {
    value: "space-with-muc",
    label: "Space + room",
    caption: "A new space and its first room.",
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
      emit("update:form", { intent: "space-muc", space_node: props.defaultSpaceNode ?? "", name: "", description: "", muc_type: "text" } satisfies CreateSpaceMucFormData);
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
  if (f.intent === "space-muc") return !f.space_node.trim() || !f.name.trim();
  if (f.intent === "space-with-muc") return !f.space_name.trim() || !f.muc_name.trim();
  return true;
});

const dialogTitle = computed(() => {
  switch (props.form.intent) {
    case "space": return "New space";
    case "muc": return "New room";
    case "space-muc": return "New room in a space";
    case "space-with-muc": return "New space and room";
  }
});
</script>

<template>
  <AppDialog v-model:open="open">
    <div :class="dialogParts.head">
      <h2>{{ dialogTitle }}</h2>
      <button :class="iconbtn()" type="button" aria-label="Close create dialog" @click="open = false">
        <X aria-hidden="true" />
      </button>
    </div>

    <div :class="dialogParts.body">
      <!-- Intent selector -->
      <div :class="f.field" role="group" aria-labelledby="create-intent-label">
        <span id="create-intent-label" :class="f.label">What would you like to create?</span>
        <div :class="f.options">
          <button
            v-for="opt in INTENTS"
            :key="opt.value"
            type="button"
            :class="option()"
            :aria-pressed="form.intent === opt.value"
            @click="selectIntent(opt.value)"
          >
            <span :class="f.optionTitle"><component :is="opt.icon" aria-hidden="true" />{{ opt.label }}</span>
            <span :class="f.caption">{{ opt.caption }}</span>
          </button>
        </div>
      </div>

      <!-- ── Space fields ── -->
      <template v-if="form.intent === 'space'">
        <div :class="f.field">
          <label for="create-space-name" :class="f.label">Name</label>
          <input
            id="create-space-name"
            :value="form.name"
            :class="f.input"
            placeholder="My Community"
            @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
          />
        </div>
        <div :class="f.field">
          <label for="create-space-desc" :class="f.label">Description</label>
          <textarea
            id="create-space-desc"
            :value="form.description"
            :class="f.textarea"
            rows="2"
            placeholder="What is this space for?"
            @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
          />
        </div>
      </template>

      <!-- ── MUC fields ── -->
      <template v-else-if="form.intent === 'muc'">
        <div :class="f.field">
          <label for="create-muc-name" :class="f.label">Name</label>
          <input
            id="create-muc-name"
            :value="form.name"
            :class="f.input"
            placeholder="general"
            @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
          />
        </div>
        <div :class="f.field">
          <label for="create-muc-desc" :class="f.label">Description</label>
          <textarea
            id="create-muc-desc"
            :value="form.description"
            :class="f.textarea"
            rows="2"
            placeholder="What is this room for?"
            @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
          />
        </div>
      </template>

      <!-- ── MUC in existing Space fields ── -->
      <template v-else-if="form.intent === 'space-muc'">
        <div :class="f.field">
          <label for="create-spacemuc-space" :class="f.label">Space</label>
          <template v-if="spaces && spaces.length > 0">
            <AppSelect
              id="create-spacemuc-space"
              :model-value="form.space_node"
              :options="spaces.map((s) => ({ value: s.node, label: s.name }))"
              placeholder="Select a space…"
              :class="f.input"
              @update:model-value="$emit('update:form', { ...form, space_node: $event })"
            />
          </template>
          <template v-else>
            <input
              id="create-spacemuc-space"
              :value="form.space_node"
              :class="f.input"
              placeholder="team-space"
              @input="$emit('update:form', { ...form, space_node: ($event.target as HTMLInputElement).value })"
            />
            <p :class="f.caption">Enter an existing Spaces service node.</p>
          </template>
        </div>
        <div :class="f.field">
          <label for="create-spacemuc-name" :class="f.label">Room name</label>
          <input
            id="create-spacemuc-name"
            :value="form.name"
            :class="f.input"
            placeholder="general"
            @input="$emit('update:form', { ...form, name: ($event.target as HTMLInputElement).value })"
          />
        </div>
        <div :class="f.field">
          <label for="create-spacemuc-desc" :class="f.label">Description</label>
          <textarea
            id="create-spacemuc-desc"
            :value="form.description"
            :class="f.textarea"
            rows="2"
            placeholder="What is this room for?"
            @input="$emit('update:form', { ...form, description: ($event.target as HTMLTextAreaElement).value })"
          />
        </div>
      </template>

      <!-- ── Space + MUC fields ── -->
      <template v-else-if="form.intent === 'space-with-muc'">
        <div :class="f.field">
          <label for="create-swm-sname" :class="f.label">Space name</label>
          <input
            id="create-swm-sname"
            :value="form.space_name"
            :class="f.input"
            placeholder="My Community"
            @input="$emit('update:form', { ...form, space_name: ($event.target as HTMLInputElement).value })"
          />
        </div>
        <div :class="f.field">
          <label for="create-swm-sdesc" :class="f.label">Space description</label>
          <textarea
            id="create-swm-sdesc"
            :value="form.space_description"
            :class="f.textarea"
            rows="2"
            placeholder="What is this space for?"
            @input="$emit('update:form', { ...form, space_description: ($event.target as HTMLTextAreaElement).value })"
          />
        </div>
        <div :class="f.field">
          <label for="create-swm-mname" :class="f.label">Room name</label>
          <input
            id="create-swm-mname"
            :value="form.muc_name"
            :class="f.input"
            placeholder="general"
            @input="$emit('update:form', { ...form, muc_name: ($event.target as HTMLInputElement).value })"
          />
        </div>
        <div :class="f.field">
          <label for="create-swm-mdesc" :class="f.label">Room description</label>
          <textarea
            id="create-swm-mdesc"
            :value="form.muc_description"
            :class="f.textarea"
            rows="2"
            placeholder="What is this room for?"
            @input="$emit('update:form', { ...form, muc_description: ($event.target as HTMLTextAreaElement).value })"
          />
        </div>
      </template>

      <!-- ── MUC subtype picker (shown for all intents that create a MUC) ── -->
      <div v-if="hasMucType" :class="f.field" role="group" aria-labelledby="create-type-label">
        <span id="create-type-label" :class="f.label">Type</span>
        <div :class="f.options">
          <button type="button" :class="option()" :aria-pressed="currentMucType === 'text'" @click="setMucType('text')">
            <span :class="f.optionTitle"><Hash aria-hidden="true" />Chat</span>
            <span :class="f.caption">Fast back-and-forth conversation.</span>
          </button>
          <button type="button" :class="option()" :aria-pressed="currentMucType === 'forum'" @click="setMucType('forum')">
            <span :class="f.optionTitle"><MessagesSquare aria-hidden="true" />Forum</span>
            <span :class="f.caption">Topics with threaded replies.</span>
          </button>
        </div>
      </div>
    </div>

    <div :class="dialogParts.foot">
      <button :class="btn()" type="button" @click="open = false">Cancel</button>
      <button :class="btn({ variant: 'primary' })" type="button" :disabled="isSubmitDisabled" @click="emit('submit')">
        {{ isSubmitting ? "Creating…" : "Create" }}
      </button>
    </div>
  </AppDialog>
</template>
