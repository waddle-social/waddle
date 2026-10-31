import { expect, mock, test } from "bun:test";
import { installMockBrowserGlobals } from "./helpers/mock-browser-storage";
import {
	listQueuedRoomMessages,
	countQueuedMessages,
} from "../src/lib/outbound-queue-store";
installMockBrowserGlobals();
import { computed, effectScope, ref, shallowRef } from "vue";
import { useChannelReadMarkers } from "../src/channels/read-markers";
import { roomKey } from "../src/lib/last-seen-store";
import { useChannelMessages } from "../src/channels/messages";
import { useRoomSync } from "../src/shell/controllers/use-room-sync";
import { useChatShellState } from "../src/shell/state";
import type { useDirectMessageConversations } from "../src/dms/conversations";
import type { ExtensionRouteKey } from "../src/shell/controllers/use-extension-routes";
import { BrowserXmppClient } from "../src/lib/xmpp-client";
import { AmbiguousRoomIdError } from "../src/lib/xmpp/room-id-bindings";
import { nullResumePersistence } from "../src/lib/xmpp/resume-persistence";
import type { WaddleSession } from "../src/lib/server-auth";
import { useWaddleDirectory } from "../src/waddles/directory";
import { resolveChannelBySlug } from "../src/shell/route-helpers";
import {
	knownChannelIdForRoomJid,
	roomJidForChannelId,
} from "../src/lib/channel-room";
import {
	discoInfoXml,
	discoItemsXml,
	pubsubItemsXml,
	withFakeDomParser,
} from "./helpers/disco-xml";
const a = "general@muc.a.test",
	b = "general@muc.b.test",
	other = "other@muc.a.test";
const session = {
	jid: "alice@a.test",
	username: "alice",
	session_id: "test",
	xmpp_websocket_url: "wss://a.test/ws",
} as WaddleSession;
function fixture() {
	const state = {
		rooms: [a],
		incomplete: false,
		legacySpace: false,
		groupRoom: "",
	};
	const effect = mock(async (..._args: unknown[]): Promise<[]> => []);
	const xmpp = {
		join_room: effect,
		send_group_message: effect,
		fetch_room_history_page: effect,
		search_room_history: effect,
		set_room_affiliation: effect,
		list_room_members: effect,
		list_room_bots: effect,
		async send_raw_iq(xml: string) {
			if (xml.includes('xmlns="http://jabber.org/protocol/disco#items"')) {
				if (xml.includes('to="a.test"'))
					return discoItemsXml([
						{ jid: "muc.a.test" },
						{ jid: "spaces.a.test" },
					]);
				if (xml.includes('to="muc.a.test"'))
					return discoItemsXml(state.rooms.map((jid) => ({ jid })));
				if (state.legacySpace && xml.includes('to="spaces.a.test"'))
					return discoItemsXml([{ node: "space" }]);
				return discoItemsXml([]);
			}
			if (xml.includes('xmlns="http://jabber.org/protocol/disco#info"')) {
				if (state.legacySpace && xml.includes('to="spaces.a.test"'))
					return discoInfoXml({
						identities: [{ category: "pubsub", type: "leaf" }],
						features: [
							"http://jabber.org/protocol/pubsub",
							"urn:xmpp:spaces:0",
						],
						fields: {
							FORM_TYPE: "http://jabber.org/protocol/pubsub#meta-data",
							"pubsub#type": "urn:xmpp:spaces:0",
						},
					});
				return discoInfoXml({
					identities: [{ category: "conference", type: "text" }],
					features: [
						"http://jabber.org/protocol/muc",
						...(state.groupRoom && xml.includes(`to="${state.groupRoom}"`)
							? ["urn:waddle:group-dm:0"]
							: []),
					],
				});
			}
			if (state.incomplete) throw new Error("bookmark outage");
			return pubsubItemsXml(state.rooms.map((id) => ({ id, autojoin: false })));
		},
	};
	const client = new BrowserXmppClient(session, nullResumePersistence);
	const internal = client as unknown as {
		xmpp: typeof xmpp;
		connected: boolean;
		discoveredRoomJids: Map<string, string>;
		roomJidForChannel(id: string): string;
	};
	internal.xmpp = xmpp;
	internal.connected = true;
	return { client, internal, state, effect, xmpp };
}
async function assertNoIdEffects(
	client: BrowserXmppClient,
	effect: ReturnType<typeof mock>,
) {
	const before = effect.mock.calls.length;
	const queuedBefore = countQueuedMessages("alice@a.test");
	await expect(
		client.sendGroupMessage("", "general", "must not enqueue"),
	).rejects.toBeInstanceOf(AmbiguousRoomIdError);
	await expect(client.queryMamPage("", "general")).rejects.toBeInstanceOf(
		AmbiguousRoomIdError,
	);
	await expect(
		client.searchMessages("", "general", "query"),
	).rejects.toBeInstanceOf(AmbiguousRoomIdError);
	await expect(
		client.setRoomAffiliation("general", "bob@a.test", "member"),
	).rejects.toBeInstanceOf(AmbiguousRoomIdError);
	await expect(
		client.listRoomMembers("general", { roomJid: a }),
	).rejects.toBeInstanceOf(AmbiguousRoomIdError);
	await expect(
		client.listRoomBots("general", { roomJid: b }),
	).rejects.toBeInstanceOf(AmbiguousRoomIdError);
	expect(effect.mock.calls.length).toBe(before);
	expect(countQueuedMessages("alice@a.test")).toBe(queuedBefore);
}
for (const rooms of [
	[a, b, other],
	[b, a, other],
]) {
	test(`discovery quarantines both colliding addresses (${rooms[0]}) before ID effects`, async () => {
		await withFakeDomParser(async () => {
			const f = fixture();
			f.state.rooms = rooms;
			const topology = await f.client.discoverTopology();
			expect(topology.rooms.map((room) => room.jid)).toEqual([other]);
			expect(
				topology.roomReconciliationAuthority.absentRoomKeysAuthoritative,
			).toBe(false);
			expect(f.client.listAmbiguousRoomIds()).toEqual(["general"]);
			expect(() => f.internal.roomJidForChannel("general")).toThrow(
				AmbiguousRoomIdError,
			);
			expect(f.internal.roomJidForChannel("other")).toBe(other);
			await assertNoIdEffects(f.client, f.effect);
			expect(f.effect).not.toHaveBeenCalled();
		});
	});
}
test("same address repeats without conflict; unrelated ID remains usable", async () => {
	await withFakeDomParser(async () => {
		const f = fixture();
		f.state.rooms = [a, a, other];
		await f.client.discoverTopology();
		expect(f.client.listAmbiguousRoomIds()).toEqual([]);
		f.client.rememberRoomJidForChannel(
			"general",
			"general@MUC.A.TEST/resource",
		);
		expect(f.internal.roomJidForChannel("general")).toBe("general@MUC.A.TEST");
		await f.client.setRoomAffiliation("other", "bob@a.test", "member");
		expect(f.effect.mock.calls.at(-1)).toEqual([other, "bob@a.test", "member"]);
	});
});
test("partial refresh cannot rebind; later omission and explicit remembers cannot clear quarantine", async () => {
	await withFakeDomParser(async () => {
		const f = fixture();
		await f.client.discoverTopology();
		f.state.rooms = [b];
		f.state.incomplete = true;
		expect((await f.client.discoverTopology()).rooms).toEqual([]);
		f.state.rooms = [a];
		f.state.incomplete = false;
		expect((await f.client.discoverTopology()).rooms).toEqual([]);
		for (const room of [a, b])
			expect(() => f.client.rememberRoomJidForChannel("general", room)).toThrow(
				AmbiguousRoomIdError,
			);
		f.internal.discoveredRoomJids.clear();
		expect(() => f.internal.roomJidForChannel("general")).toThrow(
			AmbiguousRoomIdError,
		);
		await assertNoIdEffects(f.client, f.effect);
		const fresh = fixture();
		fresh.state.rooms = [b];
		expect((await fresh.client.discoverTopology()).rooms[0]?.jid).toBe(b);
		expect(fresh.internal.roomJidForChannel("general")).toBe(b);
	});
});
test("explicit remember cannot overwrite; conflict emits once", () => {
	const f = fixture();
	f.client.rememberRoomJidForChannel("general", a);
	const event = mock(() => {});
	f.client.onRoomIdAmbiguous(event);
	for (const room of [b, a])
		expect(() => f.client.rememberRoomJidForChannel("general", room)).toThrow(
			AmbiguousRoomIdError,
		);
	expect(f.internal.discoveredRoomJids.has("general")).toBe(false);
	expect(event).toHaveBeenCalledTimes(1);
});
test("guessed local fallback does not reject unique custom-service discovery", () => {
	const f = fixture();
	expect(f.internal.roomJidForChannel("general")).toBe(a);
	f.client.rememberRoomJidForChannel("general", b);
	expect(f.internal.roomJidForChannel("general")).toBe(b);
});
test("explicit member address remains authoritative before local fallback", async () => {
	const f = fixture();
	await f.client.listRoomMembers("general", { roomJid: b });
	expect(f.internal.discoveredRoomJids.has("general")).toBe(false);
	expect(f.internal.roomJidForChannel("general")).toBe(b);
	expect(() => f.client.rememberRoomJidForChannel("general", a)).toThrow(
		AmbiguousRoomIdError,
	);
});

test("complete catalog omission cannot replace an observed address with a local guess", async () => {
	await withFakeDomParser(async () => {
		const f = fixture();
		f.state.rooms = [b, other];
		await f.client.discoverTopology();
		f.state.rooms = [other];
		await f.client.discoverTopology();
		expect(f.internal.discoveredRoomJids.has("general")).toBe(false);
		expect(f.internal.roomJidForChannel("general")).toBe(b);
		expect(f.internal.roomJidForChannel("other")).toBe(other);
		expect(f.client.isRoomIdAmbiguous("general")).toBe(false);
	});
});

test("routes and reverse lookup reject ordinary/group-DM collisions in either order", () => {
	const rooms = [
		{ id: "general", jid: a, name: "A" },
		{ id: "general", jid: b, name: "B", isGroupDm: true },
	];
	for (const order of [rooms, [...rooms].reverse()]) {
		expect(resolveChannelBySlug("general", order)).toBeUndefined();
		expect(knownChannelIdForRoomJid(a, order)).toBeNull();
		expect(knownChannelIdForRoomJid(b, order)).toBeNull();
		expect(() => roomJidForChannelId(session, order, "general")).toThrow(
			AmbiguousRoomIdError,
		);
	}
});
test("directory clears conflicted selection and rejects delayed member results", async () => {
	const f = fixture();
	const scope = effectScope();
	let release!: (members: []) => void;
	f.xmpp.list_room_members = mock(async () =>
		release
			? []
			: new Promise<[]>((resolve) => {
					release = resolve;
				}),
	);
	const directory = scope.run(() =>
		useWaddleDirectory(
			shallowRef(f.client),
			ref(session),
			String,
			ref(""),
			() => {},
		),
	)!;
	directory.channels.value = [
		{ id: "general", jid: a, name: "A" },
		{ id: "other", jid: other, name: "Other" },
	];
	directory.activeChannelId.value = "general";
	f.client.rememberRoomJidForChannel("general", a);
	const pending = directory.reloadChannelMembers("general");
	for (let i = 0; !release && i < 20; i++) await Promise.resolve();
	expect(release).toBeDefined();
	expect(() => f.client.rememberRoomJidForChannel("general", b)).toThrow(
		AmbiguousRoomIdError,
	);
	expect(directory.activeChannelId.value).toBeNull();
	expect(directory.channels.value.map((room) => room.id)).toEqual(["other"]);
	release([]);
	await pending;
	expect(directory.members.value).toEqual([]);
	expect(directory.memberLoadState.value).toBe("idle");
	directory.activeChannelId.value = "general";
	expect(directory.activeChannelId.value).toBeNull();
	expect(await directory.updateChannel()).toBe(false);
	expect(await directory.moveChannelToSpace("general", "another-space")).toBe(
		false,
	);
	scope.stop();
});

test("quarantine cancels delayed history and rejects selection before any UI or protocol effect", async () => {
	const f = fixture();
	const scope = effectScope();
	const client = shallowRef(f.client);
	const user = ref(session);
	const ui = useChatShellState();
	const directory = scope.run(() =>
		useWaddleDirectory(client, user, String, ui.actionError, () => {}),
	)!;
	directory.channels.value = [{ id: "general", jid: a, name: "A" }];
	directory.activeChannelId.value = "general";
	f.client.rememberRoomJidForChannel("general", a);
	const messages = scope.run(() =>
		useChannelMessages(
			user,
			client,
			directory.activeSpaceId,
			directory.activeChannelId,
			directory.currentChannel,
			String,
			ui.actionError,
			() => {},
		),
	)!;
	const cancel = mock(() => {});
	const roomSync = scope.run(() =>
		useRoomSync({
			ui,
			xmppClient: computed(() => client.value),
			session: computed(() => user.value),
			waddles: directory,
			messaging: messages,
			dmConversations: { closeDm: () => {} } as ReturnType<
				typeof useDirectMessageConversations
			>,
			computedChannelUnreadMap: computed(() => ({})),
			managedMucDomain: computed(() => "muc.a.test"),
			memberJidByNick: ref({}),
			activeExtensionRouteKey: ref<ExtensionRouteKey | null>(null),
			cancelPendingRoute: cancel,
			updateUrl: () => {},
		}),
	)!;
	let release!: (
		page: Awaited<ReturnType<BrowserXmppClient["queryMamPage"]>>,
	) => void;
	f.client.queryMamPage = mock(
		() =>
			new Promise((resolve) => {
				release = resolve;
			}),
	);
	f.client.fetchRoomPins = mock(async () => []);
	const pending = messages.loadMessages("", "general");
	expect(release).toBeDefined();
	expect(() => f.client.rememberRoomJidForChannel("general", b)).toThrow(
		AmbiguousRoomIdError,
	);
	release({ messages: [], complete: true });
	expect(await pending).toBe("aborted");
	expect(messages.messages.value).toEqual([]);
	expect(directory.activeChannelId.value).toBeNull();
	expect(cancel).toHaveBeenCalled();
	const effectsBefore = f.effect.mock.calls.length;
	await expect(
		roomSync.selectChannel("general", { roomJid: a }),
	).rejects.toBeInstanceOf(AmbiguousRoomIdError);
	expect(directory.activeChannelId.value).toBeNull();
	expect(roomSync.selectedChannelRoomJids.value).toEqual({});
	expect(f.effect.mock.calls.length).toBe(effectsBefore);
	scope.stop();
});

test("replacing the client detaches old quarantine events", () => {
	const old = fixture(),
		fresh = fixture();
	const scope = effectScope();
	const client = shallowRef(old.client);
	const directory = scope.run(() =>
		useWaddleDirectory(client, ref(session), String, ref(""), () => {}),
	)!;
	old.client.rememberRoomJidForChannel("general", a);
	client.value = fresh.client;
	fresh.client.rememberRoomJidForChannel("general", b);
	directory.channels.value = [{ id: "general", jid: b, name: "B" }];
	directory.activeChannelId.value = "general";
	expect(() => old.client.rememberRoomJidForChannel("general", b)).toThrow(
		AmbiguousRoomIdError,
	);
	expect(directory.activeChannelId.value).toBe("general");
	expect(directory.currentChannel.value?.jid).toBe(b);
	scope.stop();
});

test("already-qualified queued messages keep their original destination after quarantine", async () => {
	const f = fixture();
	f.internal.connected = false;
	f.client.connect = mock(async () => {
		throw new Error("offline");
	});
	f.client.rememberRoomJidForChannel("general", a);
	await f.client.sendGroupMessage("", "general", "accepted earlier", {
		id: "accepted-before-conflict",
	});
	expect(
		listQueuedRoomMessages("alice@a.test", a).map((entry) => entry.id),
	).toEqual(["accepted-before-conflict"]);
	expect(() => f.client.rememberRoomJidForChannel("general", b)).toThrow(
		AmbiguousRoomIdError,
	);
	await expect(
		f.client.sendGroupMessage("", "general", "must not enqueue", {
			id: "after-conflict",
		}),
	).rejects.toBeInstanceOf(AmbiguousRoomIdError);
	expect(
		listQueuedRoomMessages("alice@a.test", a).map((entry) => entry.id),
	).toEqual(["accepted-before-conflict"]);
	expect(listQueuedRoomMessages("alice@a.test", b)).toEqual([]);
	const send = mock(
		async (_room: string, _body: string, options: { stanza_id?: string }) =>
			options.stanza_id,
	);
	const state = f.client as unknown as {
		xmpp: { send_groupchat_message: typeof send };
		connected: boolean;
		currentRoom: string;
		joinedMucReady: Set<string>;
		flushQueuedRoomMessages(room: string): Promise<void>;
	};
	state.xmpp = { send_groupchat_message: send };
	state.connected = true;
	state.currentRoom = a;
	state.joinedMucReady.add(a);
	await state.flushQueuedRoomMessages(a);
	expect(send.mock.calls.map(([room]) => room)).toEqual([a]);
	expect(listQueuedRoomMessages("alice@a.test", b)).toEqual([]);
});

test("legacy space discovery cannot publish ambiguous IDs or bypass quarantine", async () => {
	await withFakeDomParser(async () => {
		const f = fixture();
		f.state.legacySpace = true;
		f.state.rooms = [a, b, other];
		expect(
			(await f.client.discoverSpaceChannels()).map((room) => room.jid),
		).toEqual([other]);
		expect(f.client.isRoomIdAmbiguous("general")).toBe(true);
		await assertNoIdEffects(f.client, f.effect);
	});
});

test("group-DM discovery and ordinary rooms share the same conflict boundary", async () => {
	await withFakeDomParser(async () => {
		const f = fixture();
		f.state.rooms = [a, b, other];
		f.state.groupRoom = b;
		const scope = effectScope();
		const directory = scope.run(() =>
			useWaddleDirectory(
				shallowRef(f.client),
				ref(session),
				String,
				ref(""),
				() => {},
			),
		)!;
		await directory.loadStructure(undefined, { noChannelSelect: true });
		expect(directory.channels.value.map((room) => room.jid)).toEqual([other]);
		expect(directory.groupDms.value).toEqual([]);
		expect(directory.activeChannelId.value).toBeNull();
		scope.stop();
	});
});

test("quarantine prevents delayed last-seen writes without changing other rooms", () => {
	const f = fixture();
	const markers = useChannelReadMarkers({
		xmppClient: shallowRef(f.client),
		activeSpaceId: ref(null),
		activeChannelId: ref(null),
		currentChannel: ref(null),
		messages: ref([]),
	});
	f.client.rememberRoomJidForChannel("general", a);
	markers.persistLastSeen("general", "before-conflict");
	expect(localStorage.getItem(roomKey("general"))).toBe("before-conflict");
	expect(() => f.client.rememberRoomJidForChannel("general", b)).toThrow(
		AmbiguousRoomIdError,
	);
	markers.persistLastSeen("general", "delayed-after-conflict");
	expect(localStorage.getItem(roomKey("general"))).toBe("before-conflict");
	markers.persistLastSeen("other", "unaffected");
	expect(localStorage.getItem(roomKey("other"))).toBe("unaffected");
});
