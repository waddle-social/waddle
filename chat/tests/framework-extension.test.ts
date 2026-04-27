import { describe, expect, test } from "bun:test";
import { parse, Registry } from "stanza/jxt";
import frameworkDefinitions from "../src/lib/xmpp/extensions/framework";

function newRegistry(): Registry {
  const registry = new Registry();
  registry.define(frameworkDefinitions);
  return registry;
}

describe("Waddle extension framework JXT", () => {
  test("imports envelope source, generic payload elements, and launch descriptors", () => {
    const registry = newRegistry();
    const envelope = parse(`
      <extensions xmlns='urn:waddle:extension:1' version='1'>
        <enrichment id='enrich-1'
                    plugin='links-task-board'
                    capability='message.enrich'
                    payload-ns='urn:waddle:links-task-board:1'
                    surface='message-card'
                    created='2026-04-27T10:00:00Z'>
          <source stanza-id='archive-id-456'
                  by='pub@muc.example.com'
                  body-start='5'
                  body-end='29'/>
          <payload>
            <link xmlns='urn:waddle:links-task-board:1'
                  url='https://example.org/post'
                  title='Example Post'
                  site='Example'/>
          </payload>
          <launch id='save-link'
                  plugin='links-task-board'
                  action='save-link'
                  command-node='urn:waddle:extension:1:invoke'
                  token='launch-token-save-link'
                  label='Save link'
                  expires-at='2026-04-28T10:00:00Z'>
            <context waddle-id='waddle-123'
                     room='pub@muc.example.com'
                     stanza-id='archive-id-456'/>
            <payload>
              <save-link xmlns='urn:waddle:links-task-board:1'
                         url='https://example.org/post'/>
            </payload>
          </launch>
        </enrichment>
      </extensions>
    `);

    const imported = registry.import(envelope) as {
      enrichments?: Array<{
        surface?: string;
        source?: Record<string, string>;
        payload?: { elements?: Array<{ name?: string; attributes?: Record<string, string> }> };
        launches?: Array<{
          plugin?: string;
          action?: string;
          commandNode?: string;
          token?: string;
          expiresAt?: string;
          context?: Record<string, string>;
          payload?: { elements?: Array<{ name?: string; attributes?: Record<string, string> }> };
        }>;
      }>;
    };

    const enrichment = imported.enrichments?.[0];
    expect(enrichment?.source).toEqual({
      stanzaId: "archive-id-456",
      by: "pub@muc.example.com",
      bodyStart: "5",
      bodyEnd: "29",
    });
    expect(enrichment?.surface).toBe("message-card");
    expect(enrichment?.payload?.elements?.[0]?.name).toBe("link");
    expect(enrichment?.payload?.elements?.[0]?.attributes?.xmlns).toBe("urn:waddle:links-task-board:1");
    expect(enrichment?.payload?.elements?.[0]?.attributes?.title).toBe("Example Post");
    expect(enrichment?.launches?.[0]?.plugin).toBe("links-task-board");
    expect(enrichment?.launches?.[0]?.action).toBe("save-link");
    expect(enrichment?.launches?.[0]?.commandNode).toBe("urn:waddle:extension:1:invoke");
    expect(enrichment?.launches?.[0]?.token).toBe("launch-token-save-link");
    expect(enrichment?.launches?.[0]?.expiresAt).toBe("2026-04-28T10:00:00Z");
    expect(enrichment?.launches?.[0]?.context).toEqual({
      waddleId: "waddle-123",
      room: "pub@muc.example.com",
      stanzaId: "archive-id-456",
    });
    expect(enrichment?.launches?.[0]?.payload?.elements?.[0]?.name).toBe("save-link");
    expect(enrichment?.launches?.[0]?.payload?.elements?.[0]?.attributes?.url).toBe("https://example.org/post");
  });
});
