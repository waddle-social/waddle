import { ref, type Ref } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { RosterContact } from "@/lib/xmpp/types";

export function useRosterContacts(xmppClient: Ref<BrowserXmppClient | null>) {
  const contacts = ref<RosterContact[]>([]);
  const isLoadingContacts = ref(false);

  async function loadRosterContacts() {
    const client = xmppClient.value;
    if (!client) {
      contacts.value = [];
      return;
    }

    isLoadingContacts.value = true;
    try {
      contacts.value = await client.listRosterContacts();
    } catch {
      contacts.value = [];
    } finally {
      isLoadingContacts.value = false;
    }
  }

  function updatePresence(jid: string, show: RosterContact["presenceShow"]) {
    const bareJid = jid.split("/")[0] ?? jid;
    contacts.value = contacts.value.map((contact) =>
      contact.jid === bareJid ? { ...contact, presenceShow: show } : contact,
    );
  }

  function clearRosterContacts() {
    contacts.value = [];
    isLoadingContacts.value = false;
  }

  return {
    contacts,
    isLoadingContacts,
    loadRosterContacts,
    updatePresence,
    clearRosterContacts,
  };
}
