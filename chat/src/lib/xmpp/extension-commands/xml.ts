import { NS_ADHOC_COMMANDS } from "./constants";
import { parseCommandActions } from "./form-fields";
import type { DataFormField, ExtensionCommandNote, ExtensionCommandResult } from "./types";

export function escapeXml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&apos;");
}

function dataFieldXml(name: string, type: string | undefined, value: string | string[] | boolean): string {
  const typeAttr = type ? ` type="${escapeXml(type)}"` : "";
  const values = Array.isArray(value) ? value : [String(value)];
  return `<field var="${escapeXml(name)}"${typeAttr}>${values.map((v) => `<value>${escapeXml(String(v))}</value>`).join("")}</field>`;
}

export function buildCommandIqXml(to: string, node: string, action: string, fields?: DataFormField[], sessionId?: string): string {
  const sessionAttr = sessionId ? ` sessionid="${escapeXml(sessionId)}"` : "";
  const formXml = fields?.length
    ? `<x xmlns="jabber:x:data" type="submit">${fields.map((f) => dataFieldXml(f.name, typeof f.value === "boolean" ? "boolean" : undefined, f.value)).join("")}</x>`
    : "";
  return `<iq type="set" id="${crypto.randomUUID()}" to="${escapeXml(to)}"><command xmlns="${NS_ADHOC_COMMANDS}" node="${escapeXml(node)}" action="${escapeXml(action)}"${sessionAttr}>${formXml}</command></iq>`;
}

export function decodeXml(text: string): string {
  return text
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", '"')
    .replaceAll("&apos;", "'")
    .replaceAll("&amp;", "&");
}

export function readXmlAttr(attrs: string, name: string): string | undefined {
  const match = attrs.match(new RegExp(`${name}=["']([^"']*)["']`));
  return match?.[1] ? decodeXml(match[1]) : undefined;
}

export function parseCommandIqResponse(xml: string): ExtensionCommandResult {
  const commandMatch = xml.match(/<command\b([^>]*)>([\s\S]*?)<\/command>|<command\b([^>]*)\/>/);
  const commandAttrs = commandMatch?.[1] ?? commandMatch?.[3] ?? "";
  if (!commandMatch) return { notes: [] };
  const commandBody = commandMatch[2] ?? "";
  const status = readXmlAttr(commandAttrs, "status");
  const sessionId = readXmlAttr(commandAttrs, "sessionid") ?? readXmlAttr(commandAttrs, "sid");
  const notes: ExtensionCommandNote[] = Array.from(commandBody.matchAll(/<note\b([^>]*)>([\s\S]*?)<\/note>/g))
    .map(([, attrs, value]) => ({ type: readXmlAttr(attrs, "type"), value: decodeXml(value).trim() }))
    .filter((note) => note.value.length > 0);
  const actionsMatch = commandBody.match(/<actions\b([^>]*)>([\s\S]*?)<\/actions>/);
  const actions = actionsMatch ? {
    execute: readXmlAttr(actionsMatch[1], "execute"),
    next: actionsMatch[2].includes("<next") || undefined,
    prev: actionsMatch[2].includes("<prev") || actionsMatch[2].includes("<previous") || undefined,
    complete: actionsMatch[2].includes("<complete") || undefined,
    cancel: actionsMatch[2].includes("<cancel") || undefined,
  } : undefined;
  const formMatch = commandBody.match(/<x\b([^>]*)>([\s\S]*?)<\/x>/);
  const form = formMatch ? {
    type: readXmlAttr(formMatch[1], "type") ?? "form",
    fields: Array.from(formMatch[2].matchAll(/<field\b([^>]*)>([\s\S]*?)<\/field>/g)).map(([, attrs, body]) => ({
      name: readXmlAttr(attrs, "var") ?? "",
      var: readXmlAttr(attrs, "var") ?? "",
      type: readXmlAttr(attrs, "type") ?? "text-single",
      label: readXmlAttr(attrs, "label"),
      desc: body.match(/<desc>([\s\S]*?)<\/desc>/)?.[1] ? decodeXml(body.match(/<desc>([\s\S]*?)<\/desc>/)![1]) : undefined,
      value: decodeXml(body.match(/<value>([\s\S]*?)<\/value>/)?.[1] ?? ""),
      values: Array.from(body.matchAll(/<value>([\s\S]*?)<\/value>/g)).map(([, value]) => decodeXml(value)),
      required: body.includes("<required"),
      options: Array.from(body.matchAll(/<option\b([^>]*)>([\s\S]*?)<\/option>/g)).map(([, optionAttrs, optionBody]) => ({
        label: readXmlAttr(optionAttrs, "label"),
        value: decodeXml(optionBody.match(/<value>([\s\S]*?)<\/value>/)?.[1] ?? ""),
      })),
    })),
  } : undefined;
  // Always derive actions, even without an <actions/> element: per
  // XEP-0050 "Command Actions", an executing response with no
  // <actions/> implies complete (plus the always-allowed cancel), so
  // single-stage commands stay inline instead of opening the palette.
  const parsedActions = parseCommandActions(actions, status, !!actionsMatch);
  return {
    ...(status ? { status } : {}),
    ...(sessionId ? { sessionId } : {}),
    notes,
    ...(parsedActions ? { actions: parsedActions } : {}),
    ...(form ? { form } : {}),
  };
}
