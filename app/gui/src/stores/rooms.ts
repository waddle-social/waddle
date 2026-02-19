import { ref, readonly } from 'vue';
import { defineStore } from 'pinia';
import { useWaddle, type RoomInfo, type UnlistenFn } from '../composables/useWaddle';
import { useAuthStore } from './auth';

const JOINED_ROOMS_STORAGE_KEY = 'waddle:joined-rooms';

type JoinedRoomsByAccount = Record<string, string[]>;

function accountKey(jid: string): string {
  return (jid.split('/')[0] || jid).trim().toLowerCase();
}

function loadJoinedRoomsByAccount(): JoinedRoomsByAccount {
  if (typeof window === 'undefined') return {};
  try {
    const raw = localStorage.getItem(JOINED_ROOMS_STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as JoinedRoomsByAccount;
    if (!parsed || typeof parsed !== 'object') return {};
    return parsed;
  } catch {
    return {};
  }
}

function saveJoinedRoomsByAccount(data: JoinedRoomsByAccount): void {
  if (typeof window === 'undefined') return;
  localStorage.setItem(JOINED_ROOMS_STORAGE_KEY, JSON.stringify(data));
}

export const useRoomsStore = defineStore('rooms', () => {
  const { discoverMucService, listRooms, joinRoom, leaveRoom, createRoom, deleteRoom, listen } =
    useWaddle();

  const mucService = ref<string | null>(null);
  const rooms = ref<RoomInfo[]>([]);
  const joinedRooms = ref<Set<string>>(new Set());
  const loading = ref(false);
  const error = ref<string | null>(null);

  const unlistenFns: UnlistenFn[] = [];
  let listening = false;
  let rejoining = false;

  function persistJoinedRooms(): void {
    const auth = useAuthStore();
    const key = accountKey(auth.jid);
    if (!key) return;
    const byAccount = loadJoinedRoomsByAccount();
    byAccount[key] = Array.from(joinedRooms.value);
    saveJoinedRoomsByAccount(byAccount);
  }

  function restoreJoinedRooms(): void {
    const auth = useAuthStore();
    const key = accountKey(auth.jid);
    if (!key) {
      joinedRooms.value = new Set();
      return;
    }
    const byAccount = loadJoinedRoomsByAccount();
    const restored = byAccount[key] ?? [];
    joinedRooms.value = new Set(restored);
  }

  async function rejoinKnownRooms(): Promise<void> {
    if (rejoining || joinedRooms.value.size === 0) return;
    rejoining = true;
    const auth = useAuthStore();
    const nick = auth.nickname;
    const roomsToJoin = Array.from(joinedRooms.value);
    try {
      await Promise.allSettled(
        roomsToJoin.map(async (roomJid) => {
          await joinRoom(roomJid, nick);
        }),
      );
    } finally {
      rejoining = false;
    }
  }

  async function discoverService(): Promise<string | null> {
    if (mucService.value) return mucService.value;
    try {
      const service = await discoverMucService();
      mucService.value = service;
      return service;
    } catch {
      return null;
    }
  }

  async function fetchRooms(): Promise<void> {
    loading.value = true;
    error.value = null;
    try {
      const service = await discoverService();
      if (!service) {
        error.value = 'No MUC service found on this server.';
        rooms.value = [];
        return;
      }
      rooms.value = await listRooms(service);
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err);
    } finally {
      loading.value = false;
    }
  }

  async function join(roomJid: string): Promise<void> {
    const auth = useAuthStore();
    const nick = auth.nickname;
    error.value = null;
    try {
      await joinRoom(roomJid, nick);
      joinedRooms.value = new Set([...joinedRooms.value, roomJid]);
      persistJoinedRooms();
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err);
      throw err;
    }
  }

  async function leave(roomJid: string): Promise<void> {
    error.value = null;
    try {
      await leaveRoom(roomJid);
      const next = new Set(joinedRooms.value);
      next.delete(roomJid);
      joinedRooms.value = next;
      persistJoinedRooms();
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err);
      throw err;
    }
  }

  async function create(roomName: string): Promise<void> {
    error.value = null;
    try {
      const service = await discoverService();
      if (!service) throw new Error('No MUC service found');

      const auth = useAuthStore();
      const nick = auth.nickname;
      // Sanitize room name → local part
      const localpart = roomName
        .toLowerCase()
        .replace(/[^a-z0-9_-]/g, '-')
        .replace(/-+/g, '-')
        .replace(/^-|-$/g, '');
      if (!localpart) throw new Error('Invalid room name');

      const roomJid = `${localpart}@${service}`;
      await createRoom(roomJid, nick);
      joinedRooms.value = new Set([...joinedRooms.value, roomJid]);
      persistJoinedRooms();

      // Refresh the room list
      await fetchRooms();
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err);
      throw err;
    }
  }

  async function destroy(roomJid: string): Promise<void> {
    error.value = null;
    try {
      await deleteRoom(roomJid);
      const next = new Set(joinedRooms.value);
      next.delete(roomJid);
      joinedRooms.value = next;
      persistJoinedRooms();

      // Refresh the room list
      await fetchRooms();
    } catch (err) {
      error.value = err instanceof Error ? err.message : String(err);
      throw err;
    }
  }

  function startListening(): void {
    if (listening) return;
    listening = true;

    restoreJoinedRooms();

    // If we're already connected, immediately rejoin so the server replays room history.
    void rejoinKnownRooms();
    void fetchRooms();

    const events = [
      'xmpp.muc.joined',
      'xmpp.muc.left',
      'xmpp.muc.created',
      'xmpp.muc.destroyed',
      'system.connection.established',
    ];

    for (const channel of events) {
      void listen(channel, () => {
        if (channel === 'system.connection.established') {
          void rejoinKnownRooms();
        }
        void fetchRooms();
      })
        .then((unlisten) => {
          unlistenFns.push(unlisten);
        })
        .catch(() => {
          // Transport not ready — tolerate gracefully
        });
    }
  }

  function stopListening(): void {
    while (unlistenFns.length > 0) {
      const fn = unlistenFns.pop();
      fn?.();
    }
    listening = false;
  }

  function reset(): void {
    stopListening();
    mucService.value = null;
    rooms.value = [];
    joinedRooms.value = new Set();
    loading.value = false;
    error.value = null;
  }

  return {
    mucService: readonly(mucService),
    rooms: readonly(rooms),
    joinedRooms: readonly(joinedRooms),
    loading: readonly(loading),
    error: readonly(error),
    discoverService,
    fetchRooms,
    join,
    leave,
    create,
    destroy,
    startListening,
    stopListening,
    reset,
  };
});
