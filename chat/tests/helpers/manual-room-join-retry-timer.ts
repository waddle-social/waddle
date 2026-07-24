import type { RoomJoinRetryTimer } from "../../src/lib/xmpp/room-join-retry";

export class ManualRoomJoinRetryTimer implements RoomJoinRetryTimer {
  readonly scheduledDelays: number[] = [];
  private nextId = 1;
  private readonly callbacks = new Map<number, () => void>();

  setTimeout(callback: () => void, delayMs: number): ReturnType<typeof setTimeout> {
    const id = this.nextId++;
    this.scheduledDelays.push(delayMs);
    this.callbacks.set(id, callback);
    return id as unknown as ReturnType<typeof setTimeout>;
  }

  clearTimeout(handle: ReturnType<typeof setTimeout>): void {
    this.callbacks.delete(handle as unknown as number);
  }

  runNext(): void {
    const next = this.callbacks.entries().next().value as [number, () => void] | undefined;
    if (!next) throw new Error("No room join retry is scheduled");
    const [id, callback] = next;
    this.callbacks.delete(id);
    callback();
  }

  get pendingCount(): number {
    return this.callbacks.size;
  }
}
