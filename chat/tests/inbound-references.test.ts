import { describe, expect, test } from "bun:test";
import {
  dmMessageFromArchived,
  roomMessageFromArchived,
} from "@/lib/xmpp/client";
import type { WasmArchivedMessage } from "@/lib/xmpp/wasm-types";

const baseArchivedRoom: WasmArchivedMessage = {
  mam_id: "mam-1",
  message_type: "groupchat",
  from: "room@conf.example.com/alice",
  to: "bob@example.com",
  body: "see https://example.com",
  reaction_emojis: [],
  is_muc: true,
  markup_spans: [],
  mention_uris: [],
  references: [],
  is_sticker: false,
  shared_files: [],
};

const baseArchivedDm: WasmArchivedMessage = {
  mam_id: "mam-dm-1",
  message_type: "chat",
  from: "alice@example.com/web",
  to: "bob@example.com",
  body: "see https://example.com",
  reaction_emojis: [],
  is_muc: false,
  markup_spans: [],
  mention_uris: [],
  references: [],
  is_sticker: false,
  shared_files: [],
};

describe("roomMessageFromArchived", () => {
  test("maps XEP-0372 references with anchor onto LiveRoomMessage.references", () => {
    const archived: WasmArchivedMessage = {
      ...baseArchivedRoom,
      references: [
        {
          ref_type: "data",
          uri: "https://example.com",
          begin: 4,
          end: 23,
          anchor: "https://example.com",
        },
      ],
    };

    const result = roomMessageFromArchived(archived);

    expect(result).not.toBeNull();
    expect(result?.references).toEqual([
      {
        type: "data",
        uri: "https://example.com",
        begin: 4,
        end: 23,
        anchor: "https://example.com",
      },
    ]);
  });

  test("omits anchor when absent", () => {
    const archived: WasmArchivedMessage = {
      ...baseArchivedRoom,
      references: [
        { ref_type: "mention", uri: "xmpp:bob@example.com", begin: 0, end: 4 },
      ],
    };

    const result = roomMessageFromArchived(archived);

    expect(result?.references).toEqual([
      { type: "mention", uri: "xmpp:bob@example.com", begin: 0, end: 4 },
    ]);
  });

  test("leaves references undefined when WASM payload has none", () => {
    const result = roomMessageFromArchived(baseArchivedRoom);
    expect(result?.references).toBeUndefined();
  });
});

describe("dmMessageFromArchived", () => {
  test("maps XEP-0372 references onto LiveDmMessage.references", () => {
    const archived: WasmArchivedMessage = {
      ...baseArchivedDm,
      references: [
        {
          ref_type: "data",
          uri: "https://example.com",
          begin: 4,
          end: 23,
        },
      ],
    };

    const result = dmMessageFromArchived(archived, "bob@example.com");

    expect(result).not.toBeNull();
    expect(result?.references).toEqual([
      {
        type: "data",
        uri: "https://example.com",
        begin: 4,
        end: 23,
      },
    ]);
  });
});
