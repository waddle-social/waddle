import type { PersistedQueuedDmMessage } from "@/lib/outbound-queue-store";
import {
  IndexedDbDurableOutboundStore,
  committedOrThrow,
} from "@/lib/xmpp-runtime-durable-store";

function directMessage(id: string): PersistedQueuedDmMessage {
  return {
    kind: "dm",
    id,
    createdAt: "2026-07-17T00:00:00.000Z",
    peerJid: "browser-recipient@example.com",
    body: "browser transaction",
  };
}

export type DurableBrowserResult = {
  outcomes: string[];
  ids: string[];
  revisions: [number, number];
};

declare global {
  interface Window {
    __waddleDurableFixture: {
      commitRace(): Promise<DurableBrowserResult>;
    };
  }
}

window.__waddleDurableFixture = {
  async commitRace() {
    const accountKey = `browser-${crypto.randomUUID()}@example.com`;
    const databaseName = `waddle-browser-contract-${crypto.randomUUID()}`;
    const first = new IndexedDbDurableOutboundStore({ databaseName });
    const second = new IndexedDbDurableOutboundStore({ databaseName });
    try {
      const outcomes = await Promise.all([
        first.persistReady(accountKey, directMessage("browser-message")),
        second.persistReady(accountKey, directMessage("browser-message")),
      ]);
      const messages = committedOrThrow(
        "browser-list",
        await second.list(accountKey),
      );
      const revisions: [number, number] = [
        committedOrThrow("browser-first-revision", await first.revision(accountKey)),
        committedOrThrow("browser-second-revision", await second.revision(accountKey)),
      ];
      return {
        outcomes: outcomes.map((outcome) => committedOrThrow(
          "browser-persist",
          outcome,
        ).kind).sort(),
        ids: messages.map(({ id }) => id),
        revisions,
      };
    } finally {
      await Promise.all([first.close(), second.close()]);
    }
  },
};
