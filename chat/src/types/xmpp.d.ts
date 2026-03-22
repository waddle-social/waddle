declare module "@xmpp/xml" {
  export interface XmppElement {
    attrs: Record<string, string>;
    is(name: string): boolean;
    getChild(name: string): XmppElement | null;
    getChildText(name: string): string | undefined;
    text(): string;
    toString(): string;
  }

  export default function xml(
    name: string,
    attrsOrXmlns?: Record<string, unknown> | string,
    ...children: Array<unknown>
  ): XmppElement;
}

declare module "@xmpp/client" {
  import type xml from "@xmpp/xml";

  export interface XmppClientOptions {
    service: string;
    domain?: string;
    resource?: string;
    username?: string;
    password?: string;
    timeout?: number;
  }

  export interface XmppAddress {
    toString(): string;
  }

  export interface XmppClient {
    status?: string;
    on(event: "status", listener: (status: string) => void): this;
    on(event: "error", listener: (error: Error) => void): this;
    on(event: "stanza", listener: (stanza: import("@xmpp/xml").XmppElement) => void): this;
    on(event: "online", listener: (address: XmppAddress) => void): this;
    start(): Promise<void>;
    stop(): Promise<void>;
    send(stanza: ReturnType<typeof xml>): Promise<void>;
  }

  export function client(options: XmppClientOptions): XmppClient;
  export { default as xml } from "@xmpp/xml";
}
