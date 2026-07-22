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
  // A self-closing <actions/> counts as PRESENT (Rust-parser parity):
  // it is the server saying "no forward actions", which must suppress
  // the implied `complete` below, not trigger it.
  const actionsMatch = commandBody.match(/<actions\b([^>]*)>([\s\S]*?)<\/actions>|<actions\b([^>]*)\/>/);
  const actionsAttrs = actionsMatch?.[1] ?? actionsMatch?.[3] ?? "";
  const actionsBody = actionsMatch?.[2] ?? "";
  const actions = actionsMatch ? {
    execute: readXmlAttr(actionsAttrs, "execute"),
    next: actionsBody.includes("<next") || undefined,
    prev: actionsBody.includes("<prev") || actionsBody.includes("<previous") || undefined,
    complete: actionsBody.includes("<complete") || undefined,
    cancel: actionsBody.includes("<cancel") || undefined,
  } : undefined;
  // Self-closing shapes are matched everywhere the server's minidom
  // serializer can produce them (Rust-parser parity): a childless
  // <field/> is a real optional valueless field, and <value/> is an
  // empty-string value, exactly as minidom's get_child/text see them.
  // <option/> stays dropped on both sides (no <value/> child).
  const formMatch = commandBody.match(/<x\b([^>]*)>([\s\S]*?)<\/x>|<x\b([^>]*)\/>/);
  const formAttrs = formMatch?.[1] ?? formMatch?.[3] ?? "";
  const formBody = formMatch?.[2] ?? "";
  const form = formMatch ? {
    type: readXmlAttr(formAttrs, "type") ?? "form",
    // The expanded-form alternative must reject a trailing "/" in its
    // attrs, otherwise `<field a/><field b>…</field>` would swallow
    // both repeated fields into one match.
    fields: Array.from(formBody.matchAll(/<field\b((?:[^>]*[^/>])?)>([\s\S]*?)<\/field>|<field\b([^>]*)\/>/g)).map((match) => {
      const attrs = match[1] ?? match[3] ?? "";
      const body = match[2] ?? "";
      return {
        name: readXmlAttr(attrs, "var") ?? "",
        var: readXmlAttr(attrs, "var") ?? "",
        type: readXmlAttr(attrs, "type") ?? "text-single",
        label: readXmlAttr(attrs, "label"),
        desc: body.match(/<desc>([\s\S]*?)<\/desc>/)?.[1] ? decodeXml(body.match(/<desc>([\s\S]*?)<\/desc>/)![1]) : undefined,
        value: decodeXml(body.match(/<value>([\s\S]*?)<\/value>|<value\s*\/>/)?.[1] ?? ""),
        values: Array.from(body.matchAll(/<value>([\s\S]*?)<\/value>|<value\s*\/>/g)).map(([, value]) => decodeXml(value ?? "")),
        required: body.includes("<required"),
        options: Array.from(body.matchAll(/<option\b([^>]*)>([\s\S]*?)<\/option>/g)).map(([, optionAttrs, optionBody]) => ({
          label: readXmlAttr(optionAttrs, "label"),
          value: decodeXml(optionBody.match(/<value>([\s\S]*?)<\/value>/)?.[1] ?? ""),
        })),
      };
    }),
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
