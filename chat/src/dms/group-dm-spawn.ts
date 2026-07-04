import { barePeerJid, jidLocalpart } from "@/lib/xmpp/jid";

interface GroupDmSpawnInput {
  peerJid: string;
  selfJid?: string | null;
  name?: string;
  selectedMemberJids: readonly string[];
  selectedMemberLabels?: readonly string[];
}

export interface GroupDmSpawnPayload {
  name: string;
  memberJids: string[];
}

export function groupDmSpawnPayloadFromDm(input: GroupDmSpawnInput): GroupDmSpawnPayload {
  const self = normalizedBareJid(input.selfJid ?? "");
  const memberJids = uniqueBareJids([input.peerJid, ...input.selectedMemberJids])
    .filter((jid) => jid && jid !== self);
  return {
    name: input.name?.trim() || groupDmSpawnName(input.peerJid, input.selectedMemberLabels ?? []),
    memberJids,
  };
}

function uniqueBareJids(jids: readonly string[]): string[] {
  return [...new Set(jids.map(normalizedBareJid).filter(Boolean))];
}

function normalizedBareJid(jid: string): string {
  return barePeerJid(jid).toLowerCase();
}

function groupDmSpawnName(peerJid: string, selectedMemberLabels: readonly string[]): string {
  const labels = [labelFromJid(peerJid), ...selectedMemberLabels.map((label) => label.trim()).filter(Boolean)];
  return [...new Set(labels)].join(", ");
}

function labelFromJid(jid: string): string {
  const localpart = jidLocalpart(normalizedBareJid(jid));
  return localpart ? `${localpart.charAt(0).toUpperCase()}${localpart.slice(1)}` : jid;
}
