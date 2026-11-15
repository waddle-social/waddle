<script setup lang="ts">
import { X, Search } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { iconbtn, input, seg, waddle } from "styled-system/recipes";
import AppDialog from "@/components/ui/AppDialog.vue";
import { dialogParts } from "@/ui/part-classes";
import { formClasses as f } from "@/components/modals/form-classes";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import type { MemberSummary, UserSearchResult } from "@/lib/chat-types";
import type { EditableAffiliation } from "@/lib/chat-ui";
import type { RoomPresence } from "@/lib/xmpp-client";
import type { MemberLoadState } from "@/waddles/directory";

const open = defineModel<boolean>("open", { required: true });

defineProps<{
  members: MemberSummary[];
  inferredMemberJids: Set<string>;
  memberState: MemberLoadState;
  memberQuery: string;
  newMemberAffiliation: EditableAffiliation;
  searchResults: UserSearchResult[];
  isSearching: boolean;
  canManageMembers: boolean;
  roomPresence?: RoomPresence;
  roomLastSeen?: Record<string, number>;
}>();

const emit = defineEmits<{
  "update:memberQuery": [value: string];
  "update:newMemberAffiliation": [value: EditableAffiliation];
  addMember: [userId: string];
  updateAffiliation: [member: MemberSummary, affiliation: EditableAffiliation];
  removeMember: [member: MemberSummary];
}>();

const affiliations: EditableAffiliation[] = ["member", "admin", "owner", "outcast"];

/**
 * Display label for an XEP-0045 affiliation. "Outcast" is the wire
 * value and feels harsh as a UI label — render as "Banned" instead.
 * The affiliation *value* (used in events, wire-format, type system)
 * stays "outcast" so XEP conformance is preserved per project hard rule.
 */
function affiliationLabel(affiliation: EditableAffiliation): string {
  return affiliation === "outcast" ? "Banned" : affiliation;
}
const addSection = css({ display: "flex", flexDirection: "column", gap: "2", px: "6", pt: "3", pb: "3", borderBottom: "1px solid {colors.border.soft}" });
const search = cx(input(), css({
  display: "flex",
  alignItems: "center",
  gap: "2",
  width: "100%",
  bg: "bg",
  _focusWithin: { outline: "3px solid {colors.focus}", outlineOffset: "1px" },
  "& svg": { width: "16px", height: "16px", flex: "none", color: "fg.soft" },
  "& input": { flex: 1, minWidth: 0, height: "100%", border: 0, bg: "transparent", color: "fg", font: "inherit", outline: "none" },
  "& input::placeholder": { color: "fg.soft" },
}));
const affiliationSeg = cx(seg(), css({ alignSelf: "flex-start", maxWidth: "100%", overflowX: "auto", "& > *": { textTransform: "capitalize" } }));
const results = css({ display: "flex", flexDirection: "column", gap: "0.5", mx: "-2" });
const addHint = css({ flex: "none", font: "600 13px/1 {fonts.body}", color: "action.text", textTransform: "capitalize" });
const list = css({ display: "flex", flexDirection: "column", minHeight: 0, maxHeight: "24rem", overflowY: "auto", px: "6", pt: "2", pb: "5" });
const row = css({ display: "flex", alignItems: "center", gap: "2.5", minHeight: "52px", py: "2", borderBottom: "1px solid {colors.border.soft}", _last: { borderBottom: 0 } });
const capitalize = css({ textTransform: "capitalize" });
const affiliationSelect = cx(input(), css({ height: "36px", minHeight: "36px", px: "2.5", font: "600 13px/1 {fonts.body}", textTransform: "capitalize", _coarse: { height: "44px", minHeight: "44px" } }));
const stale = css({ py: "1.5", font: "400 13px/1.4 {fonts.body}", color: "warning.text" });
const note = css({ py: "6", justifyContent: "center", textAlign: "center", font: "400 13px/1.4 {fonts.body}", color: "fg.muted" });
</script>

<template>
  <AppDialog v-model:open="open">
    <div :class="dialogParts.head">
      <h2>Members</h2>
      <button :class="iconbtn()" type="button" aria-label="Close members dialog" @click="open = false">
        <X aria-hidden="true" />
      </button>
    </div>

    <!-- Search -->
    <section v-if="canManageMembers" :class="addSection" aria-label="Add members">
      <div :class="search">
        <Search aria-hidden="true" />
        <input
          :value="memberQuery"
          placeholder="Search users to add…"
          aria-label="Search users to add"
          @input="$emit('update:memberQuery', ($event.target as HTMLInputElement).value)"
        />
      </div>

      <div :class="affiliationSeg" role="group" aria-label="Add as">
        <button
          v-for="affiliation in affiliations"
          :key="affiliation"
          type="button"
          :aria-pressed="newMemberAffiliation === affiliation"
          @click="$emit('update:newMemberAffiliation', affiliation)"
        >
          {{ affiliationLabel(affiliation) }}
        </button>
      </div>

      <!-- Search results -->
      <div v-if="searchResults.length > 0" :class="results">
        <button
          v-for="user in searchResults"
          :key="user.id"
          :class="f.pickRow"
          type="button"
          @click="emit('addMember', user.id)"
        >
          <UserAvatar :name="user.display_name || user.username" :jid="user.jid" size="sm" />
          <span :class="f.rowText">
            <span :class="f.rowName">{{ user.display_name || user.username }}</span>
            <span :class="f.rowSub">@{{ user.username }}</span>
          </span>
          <span :class="addHint">Add as {{ affiliationLabel(newMemberAffiliation) }}</span>
        </button>
      </div>

      <!-- Searching indicator: the shared three-dot waddle loader. -->
      <div v-if="isSearching" :class="f.status" role="status">
        <span :class="waddle()" aria-hidden="true"><i /><i /><i /></span>
        <span>Searching…</span>
      </div>
    </section>

    <!-- Member list -->
    <div :class="list">
      <div v-if="memberState === 'unavailable' && members.length > 0" :class="stale">
        Showing last synced members.
      </div>
      <div v-for="member in members" :key="member.jid" :class="row">
        <UserAvatar :name="member.username" :jid="member.jid" :presence="roomPresence?.[member.username] ?? 'offline'" :last-seen="roomLastSeen?.[member.username]" size="sm" />
        <div :class="f.rowText">
          <span :class="f.rowName">{{ member.username }}</span>
          <span :class="[f.rowSub, capitalize]">
            {{ inferredMemberJids.has(member.jid) ? "Present now" : affiliationLabel(member.affiliation as EditableAffiliation) }}
          </span>
        </div>
        <template v-if="canManageMembers && !inferredMemberJids.has(member.jid) && member.affiliation !== 'owner'">
          <select
            :value="member.affiliation"
            :class="affiliationSelect"
            :aria-label="`Affiliation for ${member.username}`"
            @change="emit('updateAffiliation', member, ($event.target as HTMLSelectElement).value as EditableAffiliation)"
          >
            <option v-for="affiliation in affiliations" :key="affiliation" :value="affiliation">{{ affiliationLabel(affiliation) }}</option>
          </select>
          <button
            :class="iconbtn({ size: 'sm' })"
            type="button"
            :aria-label="`Remove ${member.username}`"
            @click="emit('removeMember', member)"
          >
            <X aria-hidden="true" />
          </button>
        </template>
        <span v-else-if="inferredMemberJids.has(member.jid)" :class="f.rowSub">
          Synced from presence
        </span>
      </div>

      <div v-if="members.length === 0 && memberState === 'loading'" :class="[f.status, note]" role="status">
        <span :class="waddle()" aria-hidden="true"><i /><i /><i /></span>
        Syncing members…
      </div>
      <div v-else-if="members.length === 0 && memberState === 'unavailable'" :class="note">
        Members are unavailable right now.
      </div>
      <div v-else-if="members.length === 0" :class="note">
        No members
      </div>
    </div>
  </AppDialog>
</template>
