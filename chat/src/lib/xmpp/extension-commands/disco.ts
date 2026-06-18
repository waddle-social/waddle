import { jidDomain } from "../jid";
import {
  EXTENSION_COMMAND_FORM_TYPE,
  INVOKE_COMMAND_NODE,
  NS_ADHOC_COMMANDS,
  NS_WADDLE_EXTENSION_1,
} from "./constants";
import { formFieldValue } from "./form-fields";
import type { DiscoveredExtensionCommand, DiscoItem, ExtensionCommandScope, XmppSendIq } from "./types";
import { decodeXml, escapeXml, readXmlAttr } from "./xml";

async function rawDiscoItems(xmpp: XmppSendIq, jid: string, node?: string): Promise<DiscoItem[]> {
  if (typeof xmpp.getDiscoItems === "function") {
    const result = await xmpp.getDiscoItems(jid, node);
    return result.items ?? [];
  }
  if (typeof xmpp.send_raw_iq !== "function") return [];
  const nodeAttr = node ? ` node="${escapeXml(node)}"` : "";
  try {
    const responseXml = await xmpp.send_raw_iq(`<iq type="get" id="${crypto.randomUUID()}" to="${escapeXml(jid)}"><query xmlns="http://jabber.org/protocol/disco#items"${nodeAttr}/></iq>`);
    return Array.from(responseXml.matchAll(/<item\b([^>]*)\/?>(?:<\/item>)?/g)).map(([, attrs]) => ({
      jid: readXmlAttr(attrs, "jid"),
      node: readXmlAttr(attrs, "node"),
      name: readXmlAttr(attrs, "name"),
    }));
  } catch {
    return [];
  }
}

async function rawDiscoInfo(xmpp: XmppSendIq, jid: string): Promise<{ features: string[] }> {
  if (typeof xmpp.getDiscoInfo === "function") {
    const info = await xmpp.getDiscoInfo(jid);
    return { features: info.features ?? [] };
  }
  if (typeof xmpp.send_raw_iq !== "function") return { features: [] };
  const responseXml = await xmpp.send_raw_iq(`<iq type="get" id="${crypto.randomUUID()}" to="${escapeXml(jid)}"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>`);
  const features = Array.from(responseXml.matchAll(/<feature\b[^>]*var=["']([^"']+)["'][^>]*\/?>(?:<\/feature>)?/g))
    .map(([, value]) => decodeXml(value))
    .filter(Boolean);
  return { features };
}

export async function discoverExtensionCommands(
  xmpp: XmppSendIq,
  userJid: string,
): Promise<DiscoveredExtensionCommand[]> {
  const serviceJid = await discoverExtensionCommandService(xmpp, userJid);
  const items = await rawDiscoItems(xmpp, serviceJid, NS_ADHOC_COMMANDS);
  const filtered = items.filter((item): item is { jid?: string; node: string; name?: string } => !!item.node && item.node !== INVOKE_COMMAND_NODE);
  const commands: DiscoveredExtensionCommand[] = [];
  for (const item of filtered) {
    const itemServiceJid = item.jid ?? serviceJid;
    const node = item.node;
    let scope: ExtensionCommandScope = "global";
    let composerPrefix: string | undefined;
    let inlineField: string | undefined;
    let composerExecute = false;
    try {
      const info = await rawDiscoInfoFull(xmpp, itemServiceJid, node);
      const metadata = parseExtensionCommandMetadata(info.extensions);
      if (metadata.scope) scope = metadata.scope;
      composerPrefix = metadata.composerPrefix;
      inlineField = metadata.inlineField;
      composerExecute = metadata.composerExecute === true;
    } catch {
      // If disco#info is unavailable, fall back to global scope.
    }
    commands.push({
      serviceJid: itemServiceJid,
      node,
      name: item.name || node,
      scope,
      ...(composerPrefix !== undefined ? { composerPrefix } : {}),
      ...(inlineField !== undefined ? { inlineField } : {}),
      ...(composerExecute ? { composerExecute } : {}),
    });
  }
  return commands;
}

interface ExtensionCommandMetadata {
  scope: ExtensionCommandScope | null;
  composerPrefix?: string;
  inlineField?: string;
  composerExecute?: boolean;
}

function parseExtensionCommandMetadata(extensions: unknown[] | undefined): ExtensionCommandMetadata {
  const result: ExtensionCommandMetadata = { scope: null };
  if (!Array.isArray(extensions)) return result;
  for (const form of extensions) {
    const fields = (form as { fields?: unknown[] } | undefined)?.fields;
    if (!Array.isArray(fields)) continue;
    const formType = formFieldValue(fields, "FORM_TYPE");
    if (formType !== NS_WADDLE_EXTENSION_1 && formType !== EXTENSION_COMMAND_FORM_TYPE) continue;
    const scopeValue = formFieldValue(fields, "waddle#command_scope");
    if (scopeValue === "global" || scopeValue === "channel") result.scope = scopeValue;
    const composerPrefix = formFieldValue(fields, "waddle#composer_prefix");
    if (composerPrefix) result.composerPrefix = composerPrefix;
    const inlineField = formFieldValue(fields, "waddle#inline_field");
    if (inlineField) result.inlineField = inlineField;
    const composerExecute = formFieldValue(fields, "waddle#composer_execute");
    if (composerExecute?.toLowerCase() === "true") result.composerExecute = true;
  }
  return result;
}

export async function discoverExtensionCommandService(xmpp: XmppSendIq, userJid: string): Promise<string> {
  const domain = jidDomain(userJid);
  const fallbackServiceJid = `extensions.${domain}`;

  try {
    const items = await rawDiscoItems(xmpp, domain);
    const candidates = extensionServiceCandidates(
      domain,
      fallbackServiceJid,
      items.map((item) => item.jid).filter((jid): jid is string => !!jid),
    );
    for (const candidate of candidates) {
      try {
        const info = await rawDiscoInfo(xmpp, candidate);
        if (isWaddleExtensionServiceInfo(info.features)) return candidate;
      } catch {
        // Try the next discovered component.
      }
    }
  } catch {
    // Fall through to returning the conventional extension service JID.
  }
  return fallbackServiceJid;
}

function extensionServiceCandidates(domain: string, fallbackServiceJid: string, discoveredJids: string[]): string[] {
  const candidates: string[] = [];
  for (const jid of [...discoveredJids, fallbackServiceJid, domain]) {
    if (!candidates.includes(jid)) candidates.push(jid);
  }
  return candidates;
}

function isWaddleExtensionServiceInfo(features: string[]): boolean {
  return hasFeature(features, NS_WADDLE_EXTENSION_1) && hasFeature(features, NS_ADHOC_COMMANDS);
}

function hasFeature(features: string[], expected: string): boolean {
  return features.some((feature) => feature === expected);
}

function parseDiscoInfoExtensions(xml: string): Array<{ fields: Array<{ var: string; value?: string; values?: string[] }> }> {
  const forms: Array<{ fields: Array<{ var: string; value?: string; values?: string[] }> }> = [];
  for (const [, formContent] of xml.matchAll(/<x\b[^>]*xmlns=["']jabber:x:data["'][^>]*>([\s\S]*?)<\/x>/g)) {
    const fields: Array<{ var: string; value?: string; values?: string[] }> = [];
    for (const [, attrs, fieldContent] of formContent.matchAll(/<field\b([^>]*)>([\s\S]*?)<\/field>/g)) {
      const varName = readXmlAttr(attrs, "var");
      if (!varName) continue;
      const values = Array.from(fieldContent.matchAll(/<value[^>]*>([\s\S]*?)<\/value>/g))
        .map(([, v]) => decodeXml(v.trim()));
      fields.push({ var: varName, value: values[0], values });
    }
    forms.push({ fields });
  }
  return forms;
}

async function rawDiscoInfoFull(xmpp: XmppSendIq, jid: string, node?: string): Promise<{ features: string[]; extensions: unknown[] }> {
  if (typeof xmpp.getDiscoInfo === "function") {
    try {
      const info = await xmpp.getDiscoInfo(jid, node);
      const parsed = { features: info.features ?? [], extensions: info.extensions ?? [] };
      if (parsed.extensions.length > 0 || typeof xmpp.send_raw_iq !== "function") return parsed;
    } catch {
      if (typeof xmpp.send_raw_iq !== "function") return { features: [], extensions: [] };
    }
  }
  if (typeof xmpp.send_raw_iq !== "function") return { features: [], extensions: [] };
  const nodeAttr = node ? ` node="${escapeXml(node)}"` : "";
  try {
    const responseXml = await xmpp.send_raw_iq(
      `<iq type="get" id="${crypto.randomUUID()}" to="${escapeXml(jid)}"><query xmlns="http://jabber.org/protocol/disco#info"${nodeAttr}/></iq>`,
    );
    const features = Array.from(responseXml.matchAll(/<feature\b[^>]*var=["']([^"']+)["'][^>]*\/?>(?:<\/feature>)?/g))
      .map(([, value]) => decodeXml(value))
      .filter(Boolean);
    return { features, extensions: parseDiscoInfoExtensions(responseXml) };
  } catch {
    return { features: [], extensions: [] };
  }
}
