import { afterEach, describe, expect, test } from "bun:test";
import {
  authorBadge,
  authorBadgeTooltip,
  authorityBadge,
  descriptiveBadge,
  identityHats,
} from "../src/components/chat/message-card-badges";
import { occupantJidDirectory } from "../src/lib/avatars/author-jid";
import type { OccupantAuthority, OccupantHat } from "../src/lib/xmpp-client";

const owner: OccupantAuthority = { affiliation: "owner", role: "moderator" };
const admin: OccupantAuthority = { affiliation: "admin", role: "moderator" };
const sessionModerator: OccupantAuthority = { affiliation: "none", role: "moderator" };
const participant: OccupantAuthority = { affiliation: "none", role: "participant" };

const botHat: OccupantHat = { uri: "urn:waddle:hats:bot", title: "Bot" };
const verifiedHat: OccupantHat = { uri: "urn:waddle:hats:verified", title: "Verified" };
const unknownHat: OccupantHat = { uri: "urn:example:hats:speaker", title: "Speaker" };

describe("authorityBadge", () => {
  test("owner outranks all other layers", () => {
    expect(authorityBadge(owner)).toEqual({ label: "OWNER", colorClass: "text-warning/70", rank: 4 });
  });

  test("admin affiliation wins over moderator role", () => {
    expect(authorityBadge(admin)?.label).toBe("ADMIN");
  });

  test("session-promoted moderator still gets a badge", () => {
    expect(authorityBadge(sessionModerator)?.label).toBe("MOD");
  });

  test("plain participants and missing authority get none", () => {
    expect(authorityBadge(participant)).toBeNull();
    expect(authorityBadge(null)).toBeNull();
    expect(authorityBadge(undefined)).toBeNull();
  });
});

describe("descriptiveBadge", () => {
  test("returns null without hats", () => {
    expect(descriptiveBadge(undefined)).toBeNull();
    expect(descriptiveBadge([])).toBeNull();
  });

  test("verified outranks bot", () => {
    expect(descriptiveBadge([botHat, verifiedHat])?.label).toBe("VERIFIED");
  });

  test("unknown hats fall back to their title and muted colour", () => {
    expect(descriptiveBadge([unknownHat])).toEqual({
      label: "Speaker",
      colorClass: "text-muted-foreground",
      rank: 0,
    });
  });
});

describe("authorBadge", () => {
  test("authority outranks descriptive hats at equal-or-higher rank", () => {
    expect(authorBadge(admin, [botHat])?.label).toBe("ADMIN");
    expect(authorBadge(sessionModerator, [verifiedHat])?.label).toBe("MOD");
  });

  test("falls back to the descriptive badge without authority", () => {
    expect(authorBadge(participant, [botHat])?.label).toBe("BOT");
  });

  test("returns null when neither layer applies", () => {
    expect(authorBadge(null, [])).toBeNull();
  });
});

describe("authorBadgeTooltip", () => {
  test("lists every layer the occupant carries", () => {
    expect(authorBadgeTooltip(admin, [botHat, verifiedHat])).toBe("ADMIN · BOT · VERIFIED");
  });

  test("uses hat titles for unregistered URIs", () => {
    expect(authorBadgeTooltip(null, [unknownHat])).toBe("Speaker");
  });

  test("empty when no badges apply", () => {
    expect(authorBadgeTooltip(null, [])).toBe("");
  });
});

describe("identityHats: the badge follows the identity behind a row, not its nick", () => {
  afterEach(() => occupantJidDirectory.clear());
  const ROOM = "general@muc.example.com";
  const BOT = "helper@extensions.example.com";

  test("a known bot's row shows BOT even when the nick holds no hats (history)", () => {
    occupantJidDirectory.record(ROOM, "helper", `${BOT}/bot`, true);
    expect(descriptiveBadge(identityHats([], BOT))?.label).toBe("BOT");
    expect(descriptiveBadge(identityHats(undefined, BOT))?.label).toBe("BOT");
    // ...even when the nick now belongs to someone else with other hats.
    expect(identityHats([verifiedHat], BOT)).toEqual([botHat]);
  });

  test("a non-bot row never inherits the bot hat from a bot that now holds its nick", () => {
    occupantJidDirectory.record(ROOM, "helper", `${BOT}/bot`, true);
    expect(identityHats([botHat], "alice@example.com")).toEqual([]);
    expect(identityHats([botHat, verifiedHat], "alice@example.com")).toEqual([verifiedHat]);
  });

  test("a known bot keeps the hats its nick carries", () => {
    occupantJidDirectory.record(ROOM, "helper", `${BOT}/bot`, true);
    const hats = [{ uri: "urn:waddle:hats:bot", title: "Server bot" }, verifiedHat];
    expect(identityHats(hats, BOT)).toBe(hats);
  });

  test("without an identity the nick's hats are all there is", () => {
    expect(identityHats([botHat], null)).toEqual([botHat]);
    expect(identityHats(undefined, undefined)).toEqual([]);
  });
});
