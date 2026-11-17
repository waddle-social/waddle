<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { Globe, Loader2, RefreshCw, Search, X } from "lucide-vue-next";
import AppDialog from "@/components/ui/AppDialog.vue";
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

function toneClass(line: StatusLine): string {
  switch (line.tone) {
    case "danger": return "border-destructive/30 bg-destructive/8 text-destructive-text";
    case "warning": return "border-warning/30 bg-warning/10 text-foreground";
    case "progress": return "border-border bg-muted/40 text-muted-foreground";
    default: return "border-border bg-muted/40 text-foreground";
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
    <div class="chat-dialog-header">
      <h2 id="remote-communities-title" class="type-dialog-title truncate">
        Communities on other servers
      </h2>
      <button
        class="chat-icon-button flex-none hover:bg-muted"
        type="button"
        aria-label="Close communities on other servers"
        @click="open = false"
      >
        <X class="w-4 h-4 text-muted-foreground" />
      </button>
    </div>

    <div class="chat-dialog-stack chat-dialog-body overflow-auto">
      <p class="type-caption text-muted-foreground">
        Join a room or follow a Space hosted on another server. That server decides who may join;
        following a Space gives no access to its rooms, and each room is joined on its own.
      </p>

      <form class="chat-field-stack" @submit.prevent="submitLookup">
        <label for="remote-community-address" class="type-section-label text-muted-foreground">
          Address
        </label>
        <div class="flex items-center gap-2">
          <input
            id="remote-community-address"
            v-model="address"
            class="chat-field-control type-field flex-1"
            placeholder="room@chat.example.org or xmpp:spaces.example.org?;node=team"
            autocomplete="off"
            spellcheck="false"
          />
          <button
            class="chat-action-button chat-action-button--primary type-control"
            type="submit"
            :disabled="lookup.state === 'working'"
          >
            <Search class="h-3.5 w-3.5" aria-hidden="true" />
            Look up
          </button>
        </div>
      </form>

      <!-- What the host reports for the address -->
      <section v-if="lookup.state !== 'idle'" class="grid gap-2" aria-live="polite" aria-label="Lookup result">
        <p v-if="lookup.state === 'invalid'" class="type-caption text-destructive-text">
          {{ addressErrorMessage(lookup.error) }}
        </p>
        <p v-else-if="lookup.state === 'home'" class="type-caption text-muted-foreground">
          That address is on your own server. You'll find it with your rooms and Spaces.
        </p>
        <p v-else-if="lookup.state === 'working'" class="type-caption flex items-center gap-1.5 text-muted-foreground">
          <Loader2 class="h-3.5 w-3.5 motion-safe:animate-spin" aria-hidden="true" />
          Asking {{ lookupDomain() }}…
        </p>
        <div
          v-else-if="lookupFailure"
          class="type-caption rounded-md border px-3 py-2"
          :class="lookupFailure.failure.kind === 'outage' ? 'border-warning/30 bg-warning/10' : 'border-destructive/30 bg-destructive/8 text-destructive-text'"
        >
          <p class="type-strong">{{ lookupFailure.copy.title }}</p>
          <p>{{ lookupFailure.copy.detail }}</p>
          <button
            v-if="lookupFailure.failure.retry !== 'none'"
            class="chat-action-button chat-action-button--secondary type-control mt-2"
            type="button"
            @click="submitLookup"
          >
            <RefreshCw class="h-3.5 w-3.5" aria-hidden="true" />
            Try again
          </button>
        </div>

        <template v-else-if="found">
          <!-- A room -->
          <article v-if="foundRoom" class="grid gap-1.5 rounded-md border border-border p-3">
            <div class="type-control type-strong truncate">{{ foundRoom.name ?? foundRoom.roomJid }}</div>
            <div class="type-meta text-muted-foreground truncate">{{ foundRoom.roomJid }}</div>
            <p v-if="foundRoom.description" class="type-caption">{{ foundRoom.description }}</p>
            <div v-if="roomFlagLabels(foundRoom.flags).length > 0" class="flex flex-wrap gap-1">
              <span
                v-for="label in roomFlagLabels(foundRoom.flags)"
                :key="label"
                class="type-meta rounded-full border border-border px-2 py-0.5 text-muted-foreground"
              >{{ label }}</span>
            </div>
            <p v-if="foundRoom.parent" class="type-meta text-muted-foreground">
              The room says it belongs to the Space “{{ foundRoom.parent.node }}” on
              {{ foundRoom.parent.serviceJid }}. Joining the room does not follow that Space.
            </p>
            <div class="flex justify-end">
              <button
                class="chat-action-button chat-action-button--primary type-control"
                type="button"
                :disabled="knownRoom(foundRoom.roomJid)?.status.state === 'working'"
                @click="joinFoundRoom"
              >
                {{ knownRoom(foundRoom.roomJid)?.joined ? "Open" : "Join" }}
              </button>
            </div>
          </article>

          <!-- A Space -->
          <article v-else-if="foundSpace" class="grid gap-2 rounded-md border border-border p-3">
            <div class="flex items-center gap-2.5">
              <!-- Space images are never fetched: a placeholder stands in. -->
              <span
                class="flex h-9 w-9 flex-none items-center justify-center rounded-md bg-muted type-control type-strong text-muted-foreground"
                aria-hidden="true"
              >{{ placeholderInitial(foundSpace.description.title, foundSpace.description.node) }}</span>
              <div class="min-w-0 flex-1">
                <div class="type-control type-strong truncate">{{ foundSpace.description.title ?? foundSpace.description.node }}</div>
                <div class="type-meta text-muted-foreground truncate">{{ foundSpace.description.serviceJid }} · {{ foundSpace.description.node }}</div>
              </div>
              <button
                v-if="!followedSpace(foundSpace.description.serviceJid, foundSpace.description.node)?.following"
                class="chat-action-button chat-action-button--primary type-control"
                type="button"
                @click="followFoundSpace"
              >
                Follow
              </button>
              <span v-else class="type-meta text-muted-foreground">Following</span>
            </div>
            <p v-if="foundSpace.description.description" class="type-caption">{{ foundSpace.description.description }}</p>
            <p
              v-if="foundSpace.content && (foundSpace.content.hasAvatar || foundSpace.content.hasBanner)"
              class="type-meta text-muted-foreground"
            >
              This Space has an image. Waddle doesn't load images from other servers' Spaces.
            </p>
            <p v-if="foundSpace.contentFailure" class="type-caption text-muted-foreground">
              {{ describeRemoteFailure(foundSpace.contentFailure, 'space').title }}: the Space's rooms could not be read.
            </p>
            <ul v-else-if="foundSpace.content" class="grid gap-1">
              <li v-if="foundSpace.content.rooms.length === 0" class="type-caption text-muted-foreground">
                The Space lists no rooms you can see.
              </li>
              <li
                v-for="room in foundSpace.content.rooms"
                :key="room.roomJid"
                class="flex items-center gap-2 rounded-md bg-muted/40 px-2 py-1.5"
              >
                <div class="min-w-0 flex-1">
                  <div class="type-control truncate">{{ room.name ?? room.roomJid }}</div>
                  <div class="type-meta text-muted-foreground truncate">{{ room.roomJid }}</div>
                </div>
                <button
                  class="chat-action-button chat-action-button--secondary type-control"
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
          <article v-else-if="foundDirectory" class="grid gap-1.5 rounded-md border border-border p-3">
            <div class="type-control type-strong truncate">{{ foundDirectory.name ?? foundDirectory.jid }}</div>
            <p v-if="foundDirectory.entries.length === 0" class="type-caption text-muted-foreground">
              It lists nothing you can open.
            </p>
            <ul v-else class="grid gap-1">
              <li
                v-for="entry in foundDirectory.entries"
                :key="directoryEntryKey(entry)"
                class="flex items-center gap-2 rounded-md bg-muted/40 px-2 py-1.5"
              >
                <div class="min-w-0 flex-1">
                  <div class="type-control truncate">{{ directoryEntryTitle(entry) }}</div>
                  <div class="type-meta text-muted-foreground truncate">
                    {{ entry.kind === 'room' ? 'Room' : entry.kind === 'space' ? 'PubSub node' : 'Service' }}
                  </div>
                </div>
                <button
                  class="chat-action-button chat-action-button--secondary type-control"
                  type="button"
                  @click="lookUpAddress(directoryEntryAddress(entry))"
                >
                  Look up
                </button>
              </li>
            </ul>
          </article>

          <p v-else-if="foundOther" class="type-caption text-muted-foreground">
            {{ foundOther.jid }} answered, but it is not a room or a Space.
          </p>
        </template>
      </section>

      <!-- Spaces the user lists or follows -->
      <section class="grid gap-2" aria-label="Your Spaces on other servers">
        <h3 class="type-section-label text-muted-foreground">Your Spaces on other servers</h3>
        <p v-if="spaces.length === 0" class="type-caption text-muted-foreground">
          {{ props.remote.isLoading.value ? "Loading…" : "You don't follow any Spaces on other servers." }}
        </p>
        <article
          v-for="space in spaces"
          :key="space.key"
          class="grid gap-1.5 rounded-md border border-border p-3"
        >
          <div class="flex items-center gap-2.5">
            <span
              class="flex h-8 w-8 flex-none items-center justify-center rounded-md bg-muted type-control type-strong text-muted-foreground"
              aria-hidden="true"
            >{{ placeholderInitial(space.name, space.node) }}</span>
            <div class="min-w-0 flex-1">
              <div class="type-control type-strong truncate">{{ space.name ?? space.node }}</div>
              <div class="type-meta text-muted-foreground truncate">
                {{ space.serviceJid }} · {{ space.following ? (space.subscription === 'pending' ? "Waiting for approval" : "Following") : "Not following" }}
              </div>
            </div>
            <div class="flex flex-none gap-1">
              <template v-for="action in spaceActions({ following: space.following, listed: space.listed })" :key="action">
                <button
                  v-if="action === 'follow'"
                  class="chat-action-button chat-action-button--secondary type-control"
                  type="button"
                  :disabled="space.status.state === 'working'"
                  @click="props.remote.followSpace(space.serviceJid, space.node, space.name)"
                >Follow</button>
                <button
                  v-else-if="action === 'unsubscribe-space'"
                  class="chat-action-button chat-action-button--secondary type-control"
                  type="button"
                  :disabled="space.status.state === 'working'"
                  title="Stop receiving this Space's updates. Rooms you joined stay joined."
                  @click="props.remote.unsubscribeSpace(space.key)"
                >Unfollow</button>
                <button
                  v-else
                  class="chat-action-button type-control text-destructive-text hover:bg-destructive/10"
                  type="button"
                  :disabled="space.status.state === 'working'"
                  title="Remove this Space from your list on every device. Rooms you joined are not affected."
                  @click="props.remote.forgetSpace(space.key)"
                >Forget</button>
              </template>
            </div>
          </div>
          <template v-if="spaceStatus(space)">
            <div class="type-caption rounded-md border px-3 py-2" :class="toneClass(spaceStatus(space)!)">
              <p class="type-strong">{{ spaceStatus(space)!.title }}</p>
              <p v-if="spaceStatus(space)!.detail">{{ spaceStatus(space)!.detail }}</p>
              <p v-if="spaceStatus(space)!.retryInSeconds !== null" class="type-meta">
                Trying again in {{ spaceStatus(space)!.retryInSeconds }} s.
              </p>
              <button
                v-if="spaceStatus(space)!.canRetry"
                class="chat-action-button chat-action-button--secondary type-control mt-1.5"
                type="button"
                @click="props.remote.retrySpace(space.key)"
              >
                <RefreshCw class="h-3.5 w-3.5" aria-hidden="true" />
                Try again
              </button>
            </div>
          </template>
          <p v-if="space.note" class="type-meta text-muted-foreground">{{ space.note }}</p>
          <ul v-if="space.content && space.content.rooms.length > 0" class="grid gap-1">
            <li
              v-for="room in space.content.rooms"
              :key="room.roomJid"
              class="flex items-center gap-2 rounded-md bg-muted/40 px-2 py-1.5"
            >
              <div class="min-w-0 flex-1">
                <div class="type-control truncate">{{ room.name ?? room.roomJid }}</div>
                <div class="type-meta text-muted-foreground truncate">{{ room.roomJid }}</div>
              </div>
              <button
                class="chat-action-button chat-action-button--secondary type-control"
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
      <section v-if="roomSuggestions.length > 0" class="grid gap-2" aria-label="Suggested by your Spaces">
        <h3 class="type-section-label text-muted-foreground">Suggested by your Spaces</h3>
        <p class="type-caption text-muted-foreground">
          These rooms are on other servers. A Space listing them is not their server's word: look each up
          before you join.
        </p>
        <ul class="grid gap-1">
          <li
            v-for="suggestion in roomSuggestions"
            :key="suggestion.roomJid"
            class="flex items-center gap-2 rounded-md bg-muted/40 px-2 py-1.5"
          >
            <div class="min-w-0 flex-1">
              <div class="type-control truncate">{{ suggestion.name ?? suggestion.roomJid }}</div>
              <div class="type-meta text-muted-foreground truncate">
                {{ suggestion.roomJid }} · listed in {{ suggestionSpaceName(suggestion.spaceId) }}
              </div>
            </div>
            <button
              class="chat-action-button chat-action-button--secondary type-control"
              type="button"
              @click="lookUpAddress(suggestion.roomJid)"
            >
              <Search class="h-3.5 w-3.5" aria-hidden="true" />
              Look up
            </button>
          </li>
        </ul>
      </section>

      <!-- Rooms the user bookmarked or joined -->
      <section class="grid gap-2" aria-label="Your rooms on other servers">
        <h3 class="type-section-label text-muted-foreground">Your rooms on other servers</h3>
        <p v-if="rooms.length === 0" class="type-caption text-muted-foreground">
          {{ props.remote.isLoading.value ? "Loading…" : "You haven't joined rooms on other servers." }}
        </p>
        <article
          v-for="room in rooms"
          :key="room.roomJid"
          class="grid gap-1.5 rounded-md border border-border p-3"
        >
          <div class="flex items-center gap-2.5">
            <Globe class="h-4 w-4 flex-none text-muted-foreground" aria-hidden="true" />
            <div class="min-w-0 flex-1">
              <div class="type-control type-strong truncate">{{ room.name ?? room.roomJid }}</div>
              <div class="type-meta text-muted-foreground truncate">
                {{ room.roomJid }} · {{ room.joined ? "Joined" : room.bookmarked ? (room.autojoin ? "Joins at sign-in" : "Saved") : "Not saved" }}
              </div>
            </div>
            <div class="flex flex-none gap-1">
              <template v-for="action in roomActions({ joined: room.joined, bookmarked: room.bookmarked })" :key="action">
                <button
                  v-if="action === 'join'"
                  class="chat-action-button chat-action-button--secondary type-control"
                  type="button"
                  :disabled="room.status.state === 'working'"
                  @click="props.remote.joinAndOpenRoom(room.roomJid, room.name)"
                >Join</button>
                <button
                  v-else-if="action === 'leave-room'"
                  class="chat-action-button chat-action-button--secondary type-control"
                  type="button"
                  :disabled="room.status.state === 'working'"
                  title="Leave the room and stop joining it at sign-in. It stays in your list."
                  @click="props.remote.leaveRoom(room.roomJid)"
                >Leave</button>
                <button
                  v-else
                  class="chat-action-button type-control text-destructive-text hover:bg-destructive/10"
                  type="button"
                  :disabled="room.status.state === 'working' || room.joining"
                  :title="room.joining ? 'A join of this room is under way.' : 'Remove the room\'s bookmark on every device. Your access to the room is not changed.'"
                  @click="props.remote.forgetRoom(room.roomJid)"
                >Forget</button>
              </template>
            </div>
          </div>
          <template v-if="roomStatus(room)">
            <div class="type-caption rounded-md border px-3 py-2" :class="toneClass(roomStatus(room)!)">
              <p class="type-strong">{{ roomStatus(room)!.title }}</p>
              <p v-if="roomStatus(room)!.detail">{{ roomStatus(room)!.detail }}</p>
              <p v-if="roomStatus(room)!.retryInSeconds !== null" class="type-meta">
                Trying again in {{ roomStatus(room)!.retryInSeconds }} s.
              </p>
              <button
                v-if="roomStatus(room)!.canRetry"
                class="chat-action-button chat-action-button--secondary type-control mt-1.5"
                type="button"
                @click="props.remote.retryRoom(room.roomJid)"
              >
                <RefreshCw class="h-3.5 w-3.5" aria-hidden="true" />
                Try again
              </button>
            </div>
          </template>
          <p v-if="departureNote(room.departure)" class="type-meta text-muted-foreground">{{ departureNote(room.departure) }}</p>
          <p v-if="room.note" class="type-meta text-muted-foreground">{{ room.note }}</p>
        </article>
      </section>
    </div>
  </AppDialog>
</template>
