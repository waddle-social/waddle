// The room and Space settings for who from other servers can reach them are
// offered only while the home server advertises federation
// (`urn:waddle:s2s:0`), and are submitted only while they are offered.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { withFakeDomParser, withFakeXmlDocument } from "./helpers/disco-xml";
import { renderVueComponentWithTeleports } from "./helpers/render-vue-sfc";
import { useWaddleDirectory } from "../src/waddles/directory";
import { FIELD_ROOM_FEDERATED } from "../src/lib/xmpp/protocol-helpers";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";

async function renderEditChannelDialog(
  form: Record<string, unknown>,
  federationAvailable: boolean,
): Promise<string> {
  const { html, teleports } = await renderVueComponentWithTeleports(
    "../src/components/modals/EditChannelDialog.vue",
    {
      open: true,
      "onUpdate:open": () => undefined,
      channel: { id: "dev", jid: "dev@muc.home.test", name: "Dev" },
      form: { name: "Dev", description: "", position: 0, ...form },
      isSubmitting: false,
      spaces: [],
      canMoveChannel: true,
      federationAvailable,
    },
    import.meta.url,
  );
  return html + Object.values(teleports).join("");
}

async function renderSpaceDetailDrawer(federationAvailable: boolean): Promise<string> {
  const { html, teleports } = await renderVueComponentWithTeleports(
    "../src/components/admin/SpaceDetailDrawer.vue",
    {
      open: true,
      xmppClient: null,
      space: {
        space_jid: "eng@spaces.home.test",
        space_node: "eng",
        name: "Eng",
        description: null,
        icon_url: null,
        channel_count: 0,
        member_count: 0,
      },
      federationAvailable,
    },
    import.meta.url,
  );
  return html + Object.values(teleports).join("");
}

describe("the room's federation setting in the edit dialog", () => {
  test("is offered while the home server federates and the owner form carries it", async () => {
    const html = await renderEditChannelDialog({ federated: true }, true);
    expect(html).toContain('id="edit-channel-federated"');
    expect(html).toContain("Open this room to accounts on other servers");
  });

  test("is not rendered when the home server does not federate, whatever the owner form holds", async () => {
    for (const federated of [true, false, null, undefined]) {
      const html = await renderEditChannelDialog({ federated }, false);
      expect(html).not.toContain("edit-channel-federated");
      expect(html).not.toContain("Open this room to accounts on other servers");
      expect(html).not.toContain("Checking whether this room");
    }
  });

  test("is not offered when the owner form does not carry it", async () => {
    const html = await renderEditChannelDialog({ federated: null }, true);
    expect(html).not.toContain("edit-channel-federated");
    expect(html).not.toContain("Checking whether this room");
  });

  test("shows a note while the owner form is still being read", async () => {
    const html = await renderEditChannelDialog({ federated: undefined }, true);
    expect(html).not.toContain("edit-channel-federated");
    expect(html).toContain("Checking whether this room");
  });

  test("leaves the rest of the dialog in place when it is hidden", async () => {
    const html = await renderEditChannelDialog({ federated: true }, false);
    expect(html).toContain('id="edit-channel-name"');
    expect(html).toContain('id="edit-channel-pin-permission"');
  });
});

describe("the Space's federation setting in the admin drawer", () => {
  test("is offered while the home server federates", async () => {
    const html = await renderSpaceDetailDrawer(true);
    expect(html).toContain("<h3>Other servers</h3>");
    expect(html).toContain('id="space-federated-help"');
  });

  test("is not rendered when the home server does not federate", async () => {
    const html = await renderSpaceDetailDrawer(false);
    expect(html).not.toContain("<h3>Other servers</h3>");
    expect(html).not.toContain("space-federated-help");
    expect(html).toContain("<h3>Members</h3>");
  });
});

describe("saving a room's settings", () => {
  const OWNER_SESSION = { jid: "alice@home.test", username: "alice" } as WaddleSession;

  function ownerFormXml(federated: string): string {
    return `<iq type="result"><query xmlns="http://jabber.org/protocol/muc#owner"><x xmlns="jabber:x:data" type="form">`
      + `<field var="muc#roomconfig_roomname"><value>Dev</value></field>`
      + `<field var="${FIELD_ROOM_FEDERATED}"><value>${federated}</value></field>`
      + `</x></query></iq>`;
  }

  /** A home server whose room holds `federated`, and the owner-form submissions it received. */
  function homeServer(options: { federation: boolean | undefined; federated: "0" | "1" }) {
    let federated = options.federated;
    const submitted: string[] = [];
    const send_raw_iq = mock(async (xml: string) => {
      if (xml.includes('type="get"')) return ownerFormXml(federated);
      submitted.push(xml);
      const written = new RegExp(`var="${FIELD_ROOM_FEDERATED}"><value>([01])</value>`).exec(xml);
      if (written) federated = written[1] as "0" | "1";
      return '<iq type="result"/>';
    });
    const client = {
      listRoomMembers: async () => [],
      listRoomBots: async () => [],
      discoverTopology: async () => ({
        spaces: [],
        rooms: [{ id: "dev", name: "Dev", jid: "dev@muc.home.test", channelType: "text" as const, position: 0 }],
        serverRole: "owner",
        services: { muc: "muc.home.test", spaces: "spaces.home.test" },
        ...(options.federation === undefined ? {} : { federation: options.federation }),
      }),
      send_raw_iq,
      agent: { send_raw_iq },
    } as unknown as BrowserXmppClient;
    return { client, submitted, held: () => federated };
  }

  /** Open the room's edit dialog state, then flip the setting off and save. */
  async function saveWithSettingFlippedOff(options: { federation: boolean | undefined }) {
    const server = homeServer({ ...options, federated: "1" });
    const directory = useWaddleDirectory(
      ref<BrowserXmppClient | null>(server.client),
      ref(OWNER_SESSION),
      String,
      ref(""),
      () => {},
    );
    await directory.loadStructure("dev");
    directory.activeChannelId.value = "dev";
    await directory.loadChannelFederation();
    expect(directory.editChannelForm.value.federated).toBe(true);
    directory.editChannelForm.value = { ...directory.editChannelForm.value, name: "Dev", federated: false };
    const saved = await directory.updateChannel();
    return { server, directory, saved };
  }

  const withXml = (run: () => Promise<void>) => withFakeDomParser(() => withFakeXmlDocument(run));

  test("writes the setting when the home server federates and the owner changed it", () => withXml(async () => {
    const { server, directory, saved } = await saveWithSettingFlippedOff({ federation: true });
    expect(directory.federationAvailable.value).toBe(true);
    expect(saved).toBe(true);
    expect(server.submitted).toHaveLength(1);
    expect(server.submitted[0]).toContain(`<field var="${FIELD_ROOM_FEDERATED}"><value>0</value></field>`);
    expect(server.held()).toBe("0");
  }));

  test("leaves the setting as the server holds it when the home server does not federate", () => withXml(async () => {
    const { server, directory, saved } = await saveWithSettingFlippedOff({ federation: false });
    expect(directory.federationAvailable.value).toBe(false);
    expect(saved).toBe(true);
    expect(server.submitted).toHaveLength(1);
    expect(server.submitted[0]).toContain(`<field var="${FIELD_ROOM_FEDERATED}"><value>1</value></field>`);
    expect(server.held()).toBe("1");
  }));

  test("leaves the setting as the server holds it while federation is not yet known", () => withXml(async () => {
    const { server, directory, saved } = await saveWithSettingFlippedOff({ federation: undefined });
    expect(directory.federationAvailable.value).toBe(false);
    expect(saved).toBe(true);
    expect(server.submitted).toHaveLength(1);
    expect(server.submitted[0]).toContain(`<field var="${FIELD_ROOM_FEDERATED}"><value>1</value></field>`);
    expect(server.held()).toBe("1");
  }));
});
