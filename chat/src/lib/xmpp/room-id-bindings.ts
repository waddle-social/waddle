import { bareJidKey, barePeerJid } from "./jid";

export class AmbiguousRoomIdError extends Error {
	constructor() {
		super(
			"This channel has conflicting room addresses. Reload after correcting the room directory.",
		);
		this.name = "AmbiguousRoomIdError";
	}
}

/** Legacy local IDs are usable only while their observed address is unique.
 * Conflicts survive refreshes and reconnects for this client object's lifetime.
 * This uses the existing JID comparison convention, not a new canonical codec.
 */
export class RoomIdBindings {
	private readonly addresses = new Map<
		string,
		{ key: string; address: string }
	>();
	private readonly conflicts = new Set<string>();

	observe(id: string, address: string): boolean {
		const key = bareJidKey(address);
		if (!id || !key || this.conflicts.has(id)) return false;
		const previous = this.addresses.get(id);
		if (previous !== undefined && previous.key !== key) {
			this.conflicts.add(id);
			return true;
		}
		this.addresses.set(id, { key, address: barePeerJid(address) });
		return false;
	}

	knownAddress(id: string): string | undefined {
		this.assertAvailable(id);
		return this.addresses.get(id)?.address;
	}

	isAmbiguous(id: string): boolean {
		return this.conflicts.has(id);
	}

	assertAvailable(id: string): void {
		if (this.isAmbiguous(id)) throw new AmbiguousRoomIdError();
	}

	ambiguousIds(): readonly string[] {
		return [...this.conflicts];
	}
}

/** Stateless check for callers resolving a supplied directory snapshot. */
export function hasConflictingRoomAddresses(
	id: string,
	rooms: readonly { id: string; jid?: string }[],
): boolean {
	const addresses = new Set(
		rooms
			.filter((room) => room.id === id && room.jid)
			.map((room) => bareJidKey(room.jid!)),
	);
	return addresses.size > 1;
}
