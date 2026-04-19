/** Aggregate and register all custom Waddle XEP protocol definitions. */
import type { Agent } from "stanza";
import hats from "./hats";
import references from "./references";
import retraction from "./retraction";
import reactions from "./reactions";
import fallback from "./fallback";
import fileSharing from "./file-sharing";
import stickers from "./stickers";
import mentions from "./mentions";
import push from "./push";
import markup from "./markup";
import replies from "./replies";
import forums from "./forums";
import inbox from "./inbox";
import encryptedFile from "./encrypted-file";
import stanzaIds from "./stanza-ids";

const allDefinitions = [
  ...hats,
  ...references,
  ...retraction,
  ...reactions,
  ...fallback,
  ...fileSharing,
  ...stickers,
  ...mentions,
  ...push,
  ...markup,
  ...replies,
  ...forums,
  ...inbox,
  ...encryptedFile,
  ...stanzaIds,
];

export function registerWaddleExtensions(client: Agent): void {
  for (const def of allDefinitions) {
    client.stanzas.define(def);
  }
}
