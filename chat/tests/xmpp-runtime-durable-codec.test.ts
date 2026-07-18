import { describe, expect, test } from "bun:test";
import type {
  PersistedQueuedDmMessage,
  PersistedQueuedRoomMessage,
} from "../src/lib/outbound-queue-store";
import {
  outboundPayloadDigest,
  validatePersistedRuntimeAccount,
} from "../src/lib/xmpp-runtime/durable-codec";

const ACCOUNT = "codec@example.com";

function emptyPersistedAccount(): Record<string, unknown> {
  return {
    accountKey: ACCOUNT,
    schemaVersion: 1,
    revision: 0,
    lastAuthorityTimeMs: 0,
    lastWallClockSampleMs: 0,
    authorityEpoch: 0,
    nextOwnerGeneration: 1,
    owners: {},
    outbound: {},
    terminals: {},
    smSnapshots: {},
  };
}

function expectCorruptRuntimeAccount(value: unknown): void {
  let failure: unknown;
  try {
    validatePersistedRuntimeAccount(value, ACCOUNT);
  } catch (error) {
    failure = error;
  }
  expect(failure).toBeInstanceOf(DOMException);
  expect((failure as DOMException).name).toBe("DataError");
}

function directMessage(
  id: string,
  body = "hello",
  createdAt = "2026-07-17T00:00:00.000Z",
): PersistedQueuedDmMessage {
  return {
    kind: "dm",
    id,
    createdAt,
    peerJid: "recipient@example.com",
    body,
  };
}

function accountWithReadyMessage(
  message: PersistedQueuedDmMessage,
): Record<string, unknown> {
  return {
    ...emptyPersistedAccount(),
    outbound: {
      [message.id]: {
        identity: {
          accountKey: ACCOUNT,
          messageId: message.id,
          incarnation: "incarnation-1",
          payloadDigest: "digest-1",
        },
        lane: { kind: "direct" },
        orderKey: `${message.createdAt}\u0000${message.id}`,
        message,
        state: { kind: "ready" },
      },
    },
  };
}

function roomMessage(
  roomJid: string,
): PersistedQueuedRoomMessage {
  return {
    kind: "room",
    id: "room-message",
    createdAt: "2026-07-17T00:00:00.000Z",
    roomJid,
    body: "hello room",
  };
}

function accountWithReadyRoomMessage(
  message: PersistedQueuedRoomMessage,
  laneRoomJid = message.roomJid,
): Record<string, unknown> {
  return {
    ...emptyPersistedAccount(),
    outbound: {
      [message.id]: {
        identity: {
          accountKey: ACCOUNT,
          messageId: message.id,
          incarnation: "room-incarnation",
          payloadDigest: "room-digest",
        },
        lane: { kind: "room", roomJid: laneRoomJid },
        orderKey: `${message.createdAt}\u0000${message.id}`,
        message,
        state: { kind: "ready" },
      },
    },
  };
}

describe("durable runtime codec", () => {
  test("strictly validates the complete account graph", () => {
    expect(() => validatePersistedRuntimeAccount(
      emptyPersistedAccount(),
      ACCOUNT,
    )).not.toThrow();

    const missingEpoch = emptyPersistedAccount();
    delete missingEpoch.authorityEpoch;
    expectCorruptRuntimeAccount(missingEpoch);

    expectCorruptRuntimeAccount({
      ...emptyPersistedAccount(),
      legacyRepairMarker: true,
    });

    expectCorruptRuntimeAccount({
      ...emptyPersistedAccount(),
      nextOwnerGeneration: 2,
      owners: {
        owner: {
          ownerId: "owner",
          ownerInstanceId: "instance",
          ownerGeneration: 1,
          authorityEpoch: 0,
          leaseUntil: 45_000,
          lastRenewedAt: 0,
          legacyLease: 45_000,
        },
      },
    });

    expectCorruptRuntimeAccount({
      ...emptyPersistedAccount(),
      nextOwnerGeneration: 2,
      smSnapshots: {
        detached: {
          accountKey: ACCOUNT,
          ownerId: "detached",
          ownerGeneration: 1,
          authorityEpoch: 0,
          version: 1,
          state: null,
          savedAt: 0,
          consumed: true,
        },
      },
    });
  });

  test("rejects nested queued-message drift and noncanonical ordering", () => {
    const message = directMessage("message-1");
    expect(() => validatePersistedRuntimeAccount(
      accountWithReadyMessage(message),
      ACCOUNT,
    )).not.toThrow();

    const unknownNestedField = accountWithReadyMessage({
      ...message,
      replyTo: {
        id: "reply-1",
        author: "sender@example.com",
        body: "quoted",
      },
    });
    const outbound = unknownNestedField.outbound as Record<string, {
      message: { replyTo: Record<string, unknown> };
    }>;
    outbound[message.id]!.message.replyTo.legacyAuthorId = "legacy";
    expectCorruptRuntimeAccount(unknownNestedField);

    const noncanonicalOrder = accountWithReadyMessage(message);
    const ordered = noncanonicalOrder.outbound as Record<string, {
      orderKey: string;
    }>;
    ordered[message.id]!.orderKey = "message-1";
    expectCorruptRuntimeAccount(noncanonicalOrder);
  });

  test("rejects noncanonical timestamps, room targets, and room lanes", () => {
    for (const createdAt of [
      "not-a-timestamp",
      "2026-07-17T00:00:00Z",
      "+010000-01-01T00:00:00.000Z",
    ]) {
      expectCorruptRuntimeAccount(
        accountWithReadyMessage(directMessage("invalid-time", "hello", createdAt)),
      );
    }

    const canonicalRoom = roomMessage("room@muc.example");
    expect(() => validatePersistedRuntimeAccount(
      accountWithReadyRoomMessage(canonicalRoom),
      ACCOUNT,
    )).not.toThrow();
    expectCorruptRuntimeAccount(
      accountWithReadyRoomMessage(
        roomMessage(" Room@MUC.Example/Resource "),
        "room@muc.example",
      ),
    );
    expectCorruptRuntimeAccount(
      accountWithReadyRoomMessage(canonicalRoom, "ROOM@muc.example/Resource"),
    );
  });

  test("digests structural semantics while excluding retry metadata", async () => {
    const original = directMessage("first-id", "hello", "2026-07-17T00:00:00.000Z");
    const retry = directMessage("retry-id", "hello", "2026-07-18T00:00:00.000Z");
    const changed = directMessage("retry-id", "different", "2026-07-18T00:00:00.000Z");

    expect(await outboundPayloadDigest(retry)).toBe(
      await outboundPayloadDigest(original),
    );
    expect(await outboundPayloadDigest(changed)).not.toBe(
      await outboundPayloadDigest(original),
    );
  });
});
