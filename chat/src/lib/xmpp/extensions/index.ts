/** Aggregate and register all custom Waddle XEP protocol definitions. */
import type { Agent } from "stanza";
import hats from "./hats";
import references from "./references";
import retraction from "./retraction";
import reactions from "./reactions";
import fileSharing from "./file-sharing";
import stickers from "./stickers";
import calls from "./calls";
import mentions from "./mentions";
import push from "./push";
import markup from "./markup";

const allDefinitions = [
  ...hats,
  ...references,
  ...retraction,
  ...reactions,
  ...fileSharing,
  ...stickers,
  ...calls,
  ...mentions,
  ...push,
  ...markup,
];

export function registerWaddleExtensions(client: Agent): void {
  for (const def of allDefinitions) {
    client.stanzas.define(def);
  }
}
