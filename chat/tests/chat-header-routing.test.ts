import { describe, expect, test } from "bun:test";
import { parse, type AttributeNode, type DirectiveNode, type ElementNode, type RootNode } from "vue/compiler-sfc";

const ELEMENT_NODE = 1;
const ATTRIBUTE_NODE = 6;
const DIRECTIVE_NODE = 7;

describe("ChatHeader notification settings routing", () => {
  test("routes room notification settings as private groups before direct chats", async () => {
    const source = await Bun.file(new URL("../src/components/chat/ChatHeader.vue", import.meta.url)).text();
    const { descriptor } = parse(source);
    const buttons = findElements(descriptor.template?.ast, "NotifyModeButton");

    expect(buttons).toHaveLength(2);
    const [roomButton, dmButton] = buttons;

    expect(directiveExp(roomButton, "if")).toBe("channel?.jid");
    expect(bindExp(roomButton, "room-jid")).toBe("channel.jid");
    expect(attributeValue(roomButton, "conversation-kind")).toBe("private-group");

    expect(directiveExp(dmButton, "else-if")).toBe("dmPeer?.peerJid");
    expect(bindExp(dmButton, "room-jid")).toBe("dmPeer.peerJid");
    expect(attributeValue(dmButton, "conversation-kind")).toBe("direct-chat");
  });
});

function findElements(root: RootNode | undefined, tag: string): ElementNode[] {
  if (!root) return [];
  const matches: ElementNode[] = [];
  const visit = (node: RootNode["children"][number]) => {
    if (node.type === ELEMENT_NODE) {
      if (node.tag === tag) matches.push(node);
      node.children.forEach(visit);
    }
  };
  root.children.forEach(visit);
  return matches;
}

function directiveExp(element: ElementNode, name: string): string | undefined {
  const directive = element.props.find((prop): prop is DirectiveNode =>
    prop.type === DIRECTIVE_NODE && prop.name === name,
  );
  return directive?.exp?.loc.source;
}

function bindExp(element: ElementNode, arg: string): string | undefined {
  const directive = element.props.find((prop): prop is DirectiveNode =>
    prop.type === DIRECTIVE_NODE && prop.name === "bind" && prop.arg?.loc.source === arg,
  );
  return directive?.exp?.loc.source;
}

function attributeValue(element: ElementNode, name: string): string | undefined {
  const attribute = element.props.find((prop): prop is AttributeNode =>
    prop.type === ATTRIBUTE_NODE && prop.name === name,
  );
  return attribute?.value?.content;
}
