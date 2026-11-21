<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { Globe, Loader2, RefreshCw, Search, X } from "lucide-vue-next";
import { iconbtn } from "styled-system/recipes";
import AppDialog from "@/components/ui/AppDialog.vue";
import { spin } from "@/components/chat/composer-classes";
import { formClasses as f } from "@/components/modals/form-classes";
import { remoteCommunityClasses as r } from "@/components/modals/remote-communities-classes";
import { dialogParts } from "@/ui/part-classes";
import { describeRemoteFailure } from "@/lib/xmpp/remote-outcome";
import { communityAddressDomain, spaceNodeIri } from "@/lib/xmpp/remote-address";
import type { RemoteDirectoryEntry } from "@/lib/xmpp/remote-community";
import type { RemoteRoomSuggestion } from "@/lib/xmpp/types";
import { roomActions, spaceActions } from "@/lib/xmpp/remote-membership";
import type { RemoteCommunities, RemoteRoomEntry, RemoteSpaceEntry } from "@/waddles/remote-communities";
import {
  addressErrorMessage,
  departureNote,
  placeholderInitial,
  roomFlagLabels,
  statusLine,
  type StatusLine,
} from "@/waddles/remote-community-presentation";

const open = defineModel<boolean>("open", { required: true });

const props = defineProps<{
  remote: RemoteCommunities;
  /** Rooms on other servers that the user's home Spaces list. */
  suggestions?: readonly RemoteRoomSuggestion[];
  /** The home Spaces, to name the Space a suggestion comes from. */
  homeSpaces?: readonly { id: string; name: string }[];
}>();

const address = ref("");
const nowMs = ref(Date.now());
let clock: ReturnType<typeof setInterval> | null = null;

const lookup = computed(() => props.remote.lookup.value);
const rooms = computed(() => props.remote.rooms.value);
const spaces = computed(() => props.remote.spaces.value);

watch(open, (isOpen) => {
  if (clock !== null) clearInterval(clock);
  clock = null;
  if (!isOpen) return;
  nowMs.value = Date.now();
  clock = setInterval(() => { nowMs.value = Date.now(); }, 1000);
  void props.remote.load();
}, { immediate: true });

onBeforeUnmount(() => {
  if (clock !== null) clearInterval(clock);
});

function submitLookup() {
  void props.remote.lookUp(address.value);
}

function lookUpAddress(value: string) {
  address.value = value;
  void props.remote.lookUp(value);
}

/**
 * A room a Space lists is joined only after the user looked it up on its
 * own host; a room already in the user's list opens or joins directly.
 */
function spaceRoomAction(roomJid: string, name: string | null) {
  const known = knownRoom(roomJid);
  if (known?.joined || known?.bookmarked) {
    void props.remote.joinAndOpenRoom(roomJid, name ?? known.name);
    return;
  }
  lookUpAddress(roomJid);
}

function spaceRoomActionLabel(roomJid: string): string {
  const known = knownRoom(roomJid);
  if (known?.joined) return "Open";
  return known?.bookmarked ? "Join" : "Look up";
}

function roomStatus(room: RemoteRoomEntry): StatusLine | null {
  return statusLine(room.status, "room", nowMs.value);
}

function spaceStatus(space: RemoteSpaceEntry): StatusLine | null {
  return statusLine(space.status, "space", nowMs.value);
}

/** The `data-tone` a status notice is drawn in. */
function noticeTone(line: StatusLine): "danger" | "warning" | "neutral" {
  switch (line.tone) {
    case "danger": return "danger";
    case "warning": return "warning";
    default: return "neutral";
  }
}

function knownRoom(roomJid: string): RemoteRoomEntry | undefined {
  return rooms.value.find((room) => room.roomJid === roomJid);
}

function followedSpace(serviceJid: string, node: string): RemoteSpaceEntry | undefined {
  return spaces.value.find((space) => space.serviceJid === serviceJid && space.node === node);
}

function lookupDomain(): string {
  const current = lookup.value;
  return "address" in current ? communityAddressDomain(current.address) : "";
}

const lookupFailure = computed(() => {
  const current = lookup.value;
  if (current.state !== "failed") return null;
  const subject = current.address.kind === "room" ? "room" : current.address.kind === "space" ? "space" : "server";
  return { failure: current.failure, copy: describeRemoteFailure(current.failure, subject) };
});

const found = computed(() => (lookup.value.state === "found" ? lookup.value : null));
const foundRoom = computed(() => (found.value?.description.kind === "room" ? found.value.description : null));
const foundSpace = computed(() => {
  const current = found.value;
  if (!current || current.description.kind !== "space") return null;
  return { description: current.description, content: current.content, contentFailure: current.contentFailure };
});
const foundDirectory = computed(() => (found.value?.description.kind === "directory" ? found.value.description : null));
const foundOther = computed(() => (found.value?.description.kind === "other" ? found.value.description : null));

function joinFoundRoom() {
  const room = foundRoom.value;
  if (room) void props.remote.joinAndOpenRoom(room.roomJid, room.name);
}

function followFoundSpace() {
  const space = foundSpace.value?.description;
  if (space) void props.remote.followSpace(space.serviceJid, space.node, space.title);
}

function directoryEntryAddress(entry: RemoteDirectoryEntry): string {
  if (entry.kind === "room") return entry.roomJid;
  if (entry.kind === "space") return spaceNodeIri(entry.serviceJid, entry.node);
  return entry.jid;
}

function directoryEntryKey(entry: RemoteDirectoryEntry): string {
  return `${entry.kind}:${directoryEntryAddress(entry)}`;
}

/**
 * Rooms on other servers that home Spaces list and that the user has not
 * bookmarked. A Space's listing is its publisher's claim: each is looked up
 * on its own host before it can be joined.
 */
const roomSuggestions = computed(() => {
  const known = new Set(rooms.value.map((room) => room.roomJid));
  const seen = new Set<string>();
  return (props.suggestions ?? []).filter((suggestion) => {
    if (known.has(suggestion.roomJid) || seen.has(suggestion.roomJid)) return false;
    seen.add(suggestion.roomJid);
    return true;
  });
});

function suggestionSpaceName(spaceId: string): string {
  return props.homeSpaces?.find((space) => space.id === spaceId)?.name ?? spaceId;
}

function directoryEntryTitle(entry: RemoteDirectoryEntry): string {
  return entry.name ?? (entry.kind === "space" ? entry.node : directoryEntryAddress(entry));
}
</script>

<template>
  <AppDialog v-model:open="open" labelled-by="remote-communities-title">
    <div :class="dialogParts.head">
      <h2 id="remote-communities-title" :class="r.title">
        Communities on other servers
      </h2>
      <button :class="iconbtn()" type="button" aria-label="Close communities on other servers" @click="open = false">
        <X aria-hidden="true" />
      </button>
    </div>

    <div :class="dialogParts.body">
      <p :class="r.intro">
        Join a room or follow a Space hosted on another server. That server decides who may join;
        following a Space gives no access to its rooms, and each room is joined on its own.
      </p>

      <form :class="f.field" @submit.prevent="submitLookup">
        <label for="remote-community-address" :class="f.label">
          Address
        </label>
        <div :class="r.lookupRow">
          <input
            id="remote-community-address"
            v-model="address"
            :class="r.addressInput"
            placeholder="room@chat.example.org or xmpp:spaces.example.org?;node=team"
            autocomplete="off"
            spellcheck="false"
          />
          <button :class="r.lookupButton" type="submit" :disabled="lookup.state === 'working'">
            <Search aria-hidden="true" />
            Look up
          </button>
        </div>
      </form>

      <!-- What the host reports for the address -->
      <section v-if="lookup.state !== 'idle'" :class="r.section" aria-live="polite" aria-label="Lookup result">
        <p v-if="lookup.state === 'invalid'" :class="r.error">
          {{ addressErrorMessage(lookup.error) }}
        </p>
        <p v-else-if="lookup.state === 'home'" :class="r.caption">
          That address is on your own server. You'll find it with your rooms and Spaces.
        </p>
        <p v-else-if="lookup.state === 'working'" :class="r.working">
          <Loader2 :class="spin" aria-hidden="true" />
          Asking {{ lookupDomain() }}…
        </p>
        <div
          v-else-if="lookupFailure"
          :class="r.notice"
          :data-tone="lookupFailure.failure.kind === 'outage' ? 'warning' : 'danger'"
        >
          <p :class="r.noticeTitle">{{ lookupFailure.copy.title }}</p>
          <p>{{ lookupFailure.copy.detail }}</p>
          <button
            v-if="lookupFailure.failure.retry !== 'none'"
            :class="r.button"
            type="button"
            @click="submitLookup"
          >
            <RefreshCw aria-hidden="true" />
            Try again
          </button>
        </div>

        <template v-else-if="found">
          <!-- A room -->
          <article v-if="foundRoom" :class="r.card">
            <div :class="r.text">
              <div :class="r.name">{{ foundRoom.name ?? foundRoom.roomJid }}</div>
              <div :class="r.meta">{{ foundRoom.roomJid }}</div>
            </div>
            <p v-if="foundRoom.description" :class="r.body">{{ foundRoom.description }}</p>
            <div v-if="roomFlagLabels(foundRoom.flags).length > 0" :class="r.flags">
              <span v-for="label in roomFlagLabels(foundRoom.flags)" :key="label" :class="r.tag">{{ label }}</span>
            </div>
            <p v-if="foundRoom.parent" :class="r.note">
              The room says it belongs to the Space “{{ foundRoom.parent.node }}” on
              {{ foundRoom.parent.serviceJid }}. Joining the room does not follow that Space.
            </p>
            <div :class="r.end">
              <button
                :class="r.primaryButton"
                type="button"
                :disabled="knownRoom(foundRoom.roomJid)?.status.state === 'working'"
                @click="joinFoundRoom"
              >
                {{ knownRoom(foundRoom.roomJid)?.joined ? "Open" : "Join" }}
              </button>
            </div>
          </article>

          <!-- A Space -->
          <article v-else-if="foundSpace" :class="r.card">
            <div :class="r.head">
              <!-- Space images are never fetched: a placeholder stands in. -->
              <span :class="r.initial" aria-hidden="true">{{ placeholderInitial(foundSpace.description.title, foundSpace.description.node) }}</span>
              <div :class="r.text">
                <div :class="r.name">{{ foundSpace.description.title ?? foundSpace.description.node }}</div>
                <div :class="r.meta">{{ foundSpace.description.serviceJid }} · {{ foundSpace.description.node }}</div>
              </div>
              <button
                v-if="!followedSpace(foundSpace.description.serviceJid, foundSpace.description.node)?.following"
                :class="r.primaryButton"
                type="button"
                @click="followFoundSpace"
              >
                Follow
              </button>
              <span v-else :class="r.note">Following</span>
            </div>
            <p v-if="foundSpace.description.description" :class="r.body">{{ foundSpace.description.description }}</p>
            <p v-if="foundSpace.content && (foundSpace.content.hasAvatar || foundSpace.content.hasBanner)" :class="r.note">
              This Space has an image. Waddle doesn't load images from other servers' Spaces.
            </p>
            <p v-if="foundSpace.contentFailure" :class="r.caption">
              {{ describeRemoteFailure(foundSpace.contentFailure, 'space').title }}: the Space's rooms could not be read.
            </p>
            <ul v-else-if="foundSpace.content" :class="r.list">
              <li v-if="foundSpace.content.rooms.length === 0" :class="r.rowEmpty">
                The Space lists no rooms you can see.
              </li>
              <li v-for="room in foundSpace.content.rooms" :key="room.roomJid" :class="r.row">
                <div :class="r.text">
                  <div :class="r.name">{{ room.name ?? room.roomJid }}</div>
                  <div :class="r.meta">{{ room.roomJid }}</div>
                </div>
                <button
                  :class="r.button"
                  type="button"
                  :title="knownRoom(room.roomJid) ? undefined : 'Ask the room\'s own server about it before joining'"
                  :disabled="knownRoom(room.roomJid)?.status.state === 'working'"
                  @click="spaceRoomAction(room.roomJid, room.name)"
                >
                  {{ spaceRoomActionLabel(room.roomJid) }}
                </button>
              </li>
            </ul>
          </article>

          <!-- A server or service listing -->
          <article v-else-if="foundDirectory" :class="r.card">
            <div :class="r.name">{{ foundDirectory.name ?? foundDirectory.jid }}</div>
            <p v-if="foundDirectory.entries.length === 0" :class="r.caption">
              It lists nothing you can open.
            </p>
            <ul v-else :class="r.list">
              <li v-for="entry in foundDirectory.entries" :key="directoryEntryKey(entry)" :class="r.row">
                <div :class="r.text">
                  <div :class="r.name">{{ directoryEntryTitle(entry) }}</div>
                  <div :class="r.meta">
                    {{ entry.kind === 'room' ? 'Room' : entry.kind === 'space' ? 'PubSub node' : 'Service' }}
                  </div>
                </div>
                <button :class="r.button" type="button" @click="lookUpAddress(directoryEntryAddress(entry))">
                  Look up
                </button>
              </li>
            </ul>
          </article>

          <p v-else-if="foundOther" :class="r.caption">
            {{ foundOther.jid }} answered, but it is not a room or a Space.
          </p>
        </template>
      </section>

      <!-- Spaces the user lists or follows -->
      <section :class="r.section" aria-label="Your Spaces on other servers">
        <h3 :class="r.sectionTitle">Your Spaces on other servers</h3>
        <p v-if="spaces.length === 0" :class="r.caption">
          {{ props.remote.isLoading.value ? "Loading…" : "You don't follow any Spaces on other servers." }}
        </p>
        <article v-for="space in spaces" :key="space.key" :class="r.card">
          <div :class="r.head">
            <span :class="r.initialSm" aria-hidden="true">{{ placeholderInitial(space.name, space.node) }}</span>
            <div :class="r.text">
              <div :class="r.name">{{ space.name ?? space.node }}</div>
              <div :class="r.meta">
                {{ space.serviceJid }} · {{ space.following ? (space.subscription === 'pending' ? "Waiting for approval" : "Following") : "Not following" }}
              </div>
            </div>
            <div :class="r.actions">
              <template v-for="action in spaceActions({ following: space.following, listed: space.listed })" :key="action">
                <button
                  v-if="action === 'follow'"
                  :class="r.button"
                  type="button"
                  :disabled="space.status.state === 'working'"
                  @click="props.remote.followSpace(space.serviceJid, space.node, space.name)"
                >Follow</button>
                <button
                  v-else-if="action === 'unsubscribe-space'"
                  :class="r.button"
                  type="button"
                  :disabled="space.status.state === 'working'"
                  title="Stop receiving this Space's updates. Rooms you joined stay joined."
                  @click="props.remote.unsubscribeSpace(space.key)"
                >Unfollow</button>
                <button
                  v-else
                  :class="r.dangerButton"
                  type="button"
                  :disabled="space.status.state === 'working'"
                  title="Remove this Space from your list on every device. Rooms you joined are not affected."
                  @click="props.remote.forgetSpace(space.key)"
                >Forget</button>
              </template>
            </div>
          </div>
          <template v-if="spaceStatus(space)">
            <div :class="r.notice" :data-tone="noticeTone(spaceStatus(space)!)">
              <p :class="r.noticeTitle">{{ spaceStatus(space)!.title }}</p>
              <p v-if="spaceStatus(space)!.detail">{{ spaceStatus(space)!.detail }}</p>
              <p v-if="spaceStatus(space)!.retryInSeconds !== null" :class="r.noticeMeta">
                Trying again in {{ spaceStatus(space)!.retryInSeconds }} s.
              </p>
              <button
                v-if="spaceStatus(space)!.canRetry"
                :class="r.button"
                type="button"
                @click="props.remote.retrySpace(space.key)"
              >
                <RefreshCw aria-hidden="true" />
                Try again
              </button>
            </div>
          </template>
          <p v-if="space.note" :class="r.note">{{ space.note }}</p>
          <ul v-if="space.content && space.content.rooms.length > 0" :class="r.list">
            <li v-for="room in space.content.rooms" :key="room.roomJid" :class="r.row">
              <div :class="r.text">
                <div :class="r.name">{{ room.name ?? room.roomJid }}</div>
                <div :class="r.meta">{{ room.roomJid }}</div>
              </div>
              <button
                :class="r.button"
                type="button"
                :title="knownRoom(room.roomJid) ? undefined : 'Ask the room\'s own server about it before joining'"
                :disabled="knownRoom(room.roomJid)?.status.state === 'working'"
                @click="spaceRoomAction(room.roomJid, room.name)"
              >
                {{ spaceRoomActionLabel(room.roomJid) }}
              </button>
            </li>
          </ul>
        </article>
      </section>

      <!-- Rooms on other servers that home Spaces list: suggestions only -->
      <section v-if="roomSuggestions.length > 0" :class="r.section" aria-label="Suggested by your Spaces">
        <h3 :class="r.sectionTitle">Suggested by your Spaces</h3>
        <p :class="r.caption">
          These rooms are on other servers. A Space listing them is not their server's word: look each up
          before you join.
        </p>
        <ul :class="r.list">
          <li v-for="suggestion in roomSuggestions" :key="suggestion.roomJid" :class="r.row">
            <div :class="r.text">
              <div :class="r.name">{{ suggestion.name ?? suggestion.roomJid }}</div>
              <div :class="r.meta">
                {{ suggestion.roomJid }} · listed in {{ suggestionSpaceName(suggestion.spaceId) }}
              </div>
            </div>
            <button :class="r.button" type="button" @click="lookUpAddress(suggestion.roomJid)">
              <Search aria-hidden="true" />
              Look up
            </button>
          </li>
        </ul>
      </section>

      <!-- Rooms the user bookmarked or joined -->
      <section :class="r.section" aria-label="Your rooms on other servers">
        <h3 :class="r.sectionTitle">Your rooms on other servers</h3>
        <p v-if="rooms.length === 0" :class="r.caption">
          {{ props.remote.isLoading.value ? "Loading…" : "You haven't joined rooms on other servers." }}
        </p>
        <article v-for="room in rooms" :key="room.roomJid" :class="r.card">
          <div :class="r.head">
            <Globe :class="r.globe" aria-hidden="true" />
            <div :class="r.text">
              <div :class="r.name">{{ room.name ?? room.roomJid }}</div>
              <div :class="r.meta">
                {{ room.roomJid }} · {{ room.joined ? "Joined" : room.bookmarked ? (room.autojoin ? "Joins at sign-in" : "Saved") : "Not saved" }}
              </div>
            </div>
            <div :class="r.actions">
              <template v-for="action in roomActions({ joined: room.joined, bookmarked: room.bookmarked })" :key="action">
                <button
                  v-if="action === 'join'"
                  :class="r.button"
                  type="button"
                  :disabled="room.status.state === 'working'"
                  @click="props.remote.joinAndOpenRoom(room.roomJid, room.name)"
                >Join</button>
                <button
                  v-else-if="action === 'leave-room'"
                  :class="r.button"
                  type="button"
                  :disabled="room.status.state === 'working'"
                  title="Leave the room and stop joining it at sign-in. It stays in your list."
                  @click="props.remote.leaveRoom(room.roomJid)"
                >Leave</button>
                <button
                  v-else
                  :class="r.dangerButton"
                  type="button"
                  :disabled="room.status.state === 'working' || room.joining"
                  :title="room.joining ? 'A join of this room is under way.' : 'Remove the room\'s bookmark on every device. Your access to the room is not changed.'"
                  @click="props.remote.forgetRoom(room.roomJid)"
                >Forget</button>
              </template>
            </div>
          </div>
          <template v-if="roomStatus(room)">
            <div :class="r.notice" :data-tone="noticeTone(roomStatus(room)!)">
              <p :class="r.noticeTitle">{{ roomStatus(room)!.title }}</p>
              <p v-if="roomStatus(room)!.detail">{{ roomStatus(room)!.detail }}</p>
              <p v-if="roomStatus(room)!.retryInSeconds !== null" :class="r.noticeMeta">
                Trying again in {{ roomStatus(room)!.retryInSeconds }} s.
              </p>
              <button
                v-if="roomStatus(room)!.canRetry"
                :class="r.button"
                type="button"
                @click="props.remote.retryRoom(room.roomJid)"
              >
                <RefreshCw aria-hidden="true" />
                Try again
              </button>
            </div>
          </template>
          <p v-if="departureNote(room.departure)" :class="r.note">{{ departureNote(room.departure) }}</p>
          <p v-if="room.note" :class="r.note">{{ room.note }}</p>
        </article>
      </section>
    </div>
  </AppDialog>
</template>
