/**
 * Custom JXT (JSON/XML Translation) definitions for XEPs not built into stanza.
 *
 * Also overrides stanza's outdated XEP-0317 Hats definition to match the
 * current spec (uri/title attrs instead of name/displayName).
 */
import type { Agent } from "stanza";
import type { DefinitionOptions } from "stanza/jxt";
import {
  attribute,
  childAttribute,
  childBoolean,
  childText,
  multipleChildText,
  splicePath,
} from "stanza/jxt";
import { NS_HATS_0 } from "stanza/Namespaces";

// -- Namespace constants for XEPs not in stanza --

const NS_REFERENCE_0 = "urn:xmpp:reference:0";
const NS_MESSAGE_RETRACT_1 = "urn:xmpp:message-retract:1";
const NS_MESSAGE_MODERATE_1 = "urn:xmpp:message-moderate:1";
const NS_FASTEN_0 = "urn:xmpp:fasten:0";
const NS_REACTIONS_0 = "urn:xmpp:reactions:0";
const NS_SFS_0 = "urn:xmpp:sfs:0";
const NS_FILE_METADATA_0 = "urn:xmpp:file:metadata:0";
const NS_URL_DATA = "http://jabber.org/protocol/url-data";
const NS_STICKERS_0 = "urn:xmpp:stickers:0";
const NS_CALL_INVITES_0 = "urn:xmpp:call-invites:0";
const NS_ONLINE_MEETINGS_0 = "urn:xmpp:http:online-meetings:invite:0";
const NS_EXPLICIT_MENTIONS_0 = "urn:xmpp:emn:0";

// -- Custom protocol definitions --

const definitions: DefinitionOptions[] = [
  // ---------------------------------------------------------------
  // XEP-0317: Hats (override stanza's outdated version)
  // The current XEP-0317 spec uses uri/title attributes on <hat>,
  // but stanza's built-in maps name/displayName. Override.
  // ---------------------------------------------------------------
  {
    element: "hat",
    fields: {
      uri: attribute("uri"),
      title: attribute("title"),
    },
    namespace: NS_HATS_0,
    path: "hat",
  },

  // ---------------------------------------------------------------
  // XEP-0372: References (mentions)
  // <reference xmlns="urn:xmpp:reference:0" type="mention" uri="xmpp:nick" begin="0" end="5"/>
  // ---------------------------------------------------------------
  {
    aliases: [{ path: "message.references", multiple: true }],
    element: "reference",
    fields: {
      type: attribute("type"),
      uri: attribute("uri"),
      begin: attribute("begin"),
      end: attribute("end"),
    },
    namespace: NS_REFERENCE_0,
  },

  // ---------------------------------------------------------------
  // XEP-0424: Message Retraction
  // <retract xmlns="urn:xmpp:message-retract:1" id="msg-id"/>
  // ---------------------------------------------------------------
  {
    aliases: [{ path: "message.retract", multiple: false }],
    element: "retract",
    fields: {
      id: attribute("id"),
    },
    namespace: NS_MESSAGE_RETRACT_1,
  },

  // ---------------------------------------------------------------
  // XEP-0425: Message Moderation (via XEP-0422 Fastening)
  // <apply-to xmlns="urn:xmpp:fasten:0" id="target-id">
  //   <moderated xmlns="urn:xmpp:message-moderate:1">
  //     <retract xmlns="urn:xmpp:message-retract:1"/>
  //     <reason>...</reason>
  //   </moderated>
  // </apply-to>
  // ---------------------------------------------------------------
  {
    aliases: [{ path: "message.applyTo", multiple: false }],
    element: "apply-to",
    fields: {
      id: attribute("id"),
    },
    namespace: NS_FASTEN_0,
  },
  {
    aliases: [{ path: "message.applyTo.moderated", multiple: false }],
    element: "moderated",
    fields: {
      retract: childBoolean(NS_MESSAGE_RETRACT_1, "retract"),
      reason: childText(NS_MESSAGE_MODERATE_1, "reason"),
    },
    namespace: NS_MESSAGE_MODERATE_1,
    path: "moderated",
  },

  // ---------------------------------------------------------------
  // XEP-0444: Message Reactions
  // <reactions xmlns="urn:xmpp:reactions:0" id="msg-id">
  //   <reaction>emoji</reaction>
  // </reactions>
  // ---------------------------------------------------------------
  {
    aliases: [{ path: "message.reactions", multiple: false }],
    element: "reactions",
    fields: {
      id: attribute("id"),
      items: multipleChildText(NS_REACTIONS_0, "reaction"),
    },
    namespace: NS_REACTIONS_0,
  },

  // ---------------------------------------------------------------
  // XEP-0446/0447: Stateless File Sharing
  // <file-sharing xmlns="urn:xmpp:sfs:0" disposition="inline">
  //   <file xmlns="urn:xmpp:file:metadata:0">
  //     <name>...</name>
  //     <media-type>...</media-type>
  //     <size>...</size>
  //     <width>...</width>
  //     <height>...</height>
  //     <desc>...</desc>
  //   </file>
  //   <sources>
  //     <url-data xmlns="http://jabber.org/protocol/url-data" target="..."/>
  //   </sources>
  // </file-sharing>
  // ---------------------------------------------------------------
  {
    aliases: [{ path: "message.fileSharing", multiple: false }],
    element: "file-sharing",
    fields: {
      disposition: attribute("disposition"),
      name: childText(NS_FILE_METADATA_0, "name"),
      mediaType: childText(NS_FILE_METADATA_0, "media-type"),
      size: childText(NS_FILE_METADATA_0, "size"),
      width: childText(NS_FILE_METADATA_0, "width"),
      height: childText(NS_FILE_METADATA_0, "height"),
      desc: childText(NS_FILE_METADATA_0, "desc"),
      url: childAttribute(NS_URL_DATA, "url-data", "target"),
    },
    namespace: NS_SFS_0,
  },

  // ---------------------------------------------------------------
  // XEP-0449: Stickers
  // <sticker xmlns="urn:xmpp:stickers:0" pack="..."/>
  // ---------------------------------------------------------------
  {
    aliases: [{ path: "message.sticker", multiple: false }],
    element: "sticker",
    fields: {
      pack: attribute("pack"),
    },
    namespace: NS_STICKERS_0,
  },

  // ---------------------------------------------------------------
  // XEP-0482: Call Invites
  // <propose xmlns="urn:xmpp:call-invites:0" id="session-id">
  //   <audio/><video/><external uri="..."/>
  // </propose>
  // ---------------------------------------------------------------
  {
    aliases: [{ path: "message.callPropose", multiple: false }],
    element: "propose",
    fields: {
      id: attribute("id"),
      audio: childBoolean(NS_CALL_INVITES_0, "audio"),
      video: childBoolean(NS_CALL_INVITES_0, "video"),
      externalUri: childAttribute(NS_CALL_INVITES_0, "external", "uri"),
    },
    namespace: NS_CALL_INVITES_0,
  },

  // ---------------------------------------------------------------
  // XEP-0483: Online Meetings
  // <meeting xmlns="urn:xmpp:http:online-meetings:invite:0" type="jitsi" url="..." desc="..."/>
  // ---------------------------------------------------------------
  {
    aliases: [{ path: "message.meeting", multiple: false }],
    element: "meeting",
    fields: {
      type: attribute("type"),
      url: attribute("url"),
      desc: attribute("desc"),
    },
    namespace: NS_ONLINE_MEETINGS_0,
  },

  // ---------------------------------------------------------------
  // XEP-0513: Explicit Mentions
  // <mentions xmlns="urn:xmpp:emn:0">
  //   <mention type="everyone"/>
  // </mentions>
  // ---------------------------------------------------------------
  {
    aliases: [{ path: "message.explicitMentions", multiple: false }],
    element: "mentions",
    fields: {
      items: splicePath(NS_EXPLICIT_MENTIONS_0, "mentions", "explicitMention", true),
    },
    namespace: NS_EXPLICIT_MENTIONS_0,
  },
  {
    element: "mention",
    fields: {
      type: attribute("type"),
    },
    namespace: NS_EXPLICIT_MENTIONS_0,
    path: "explicitMention",
  },
];

/**
 * Register all custom Waddle XMPP protocol extensions with a stanza client.
 */
export function registerWaddleExtensions(client: Agent): void {
  for (const def of definitions) {
    client.stanzas.define(def);
  }
}
