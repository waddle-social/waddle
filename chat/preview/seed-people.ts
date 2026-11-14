import type { ChatAppController } from "@/shell/chat-app-controller";

const HOST = "waddle.test";
const jid = (name: string) => `${name}@${HOST}`;

const PEOPLE: [string, string, "available" | "away" | "dnd" | "offline"][] = [
  ["jo", "Jo Hartmann", "available"],
  ["priya", "Priya", "available"],
  ["ana", "Ana Lindqvist", "available"],
  ["theo", "Theo", "available"],
  ["lior", "Lior", "dnd"],
  ["mika", "Mika", "away"],
  ["kim", "Kim", "away"],
  ["noor", "Noor", "offline"],
];

/**
 * Preview fixtures for members-*, rooms-* and dialog-* screens:
 *   members | members-room | members-empty | members-manage | members-manage-member
 *   rooms | rooms-empty | rooms-empty-member | rooms-loading
 *   dialog-create | dialog-edit | dialog-waddle | dialog-newdm | dialog-newgroup | notify
 */
export function seedPeople(c: ChatAppController, screen: string) {
  if (!/^(members|rooms|dialog-|notify)/.test(screen)) return;

  if (screen === "members-empty") {
    c.rosterContacts.contacts.value = [];
    c.dmConversations.conversations.value = [];
  } else {
    // The roster loader clears contacts when no client is wired; set them after it runs.
    setTimeout(() => (c.rosterContacts.contacts.value = contacts), 50);
  }
  const contacts = PEOPLE.map(([local, name, show]) => ({
      jid: jid(local), name, username: local, subscription: "both", groups: [],
    presenceShow: show as never,
  }));

  const roomMembers = PEOPLE.map(([local], i) => ({
    jid: jid(local), username: local, affiliation: i === 0 ? "owner" : i === 5 ? "admin" : "member",
  })) as typeof c.waddles.members.value;
  roomMembers.push({ jid: jid("spam-bot-42"), username: "spam-bot-42", affiliation: "outcast" } as never);

  const member = screen.endsWith("-member");
  c.waddles.serverRole.value = member ? null : "owner";
  if (member) c.waddles.waddles.value = [{ id: "s1", name: "Polar Station", role: "member" }];

  if (screen.startsWith("rooms-empty") || screen === "rooms-loading") {
    c.waddles.channels.value = [];
    if (screen === "rooms-loading") c.waddles.isLoadingStructure.value = true;
  }

  const inRoom = ["members-room", "members-manage", "members-manage-member", "dialog-edit", "notify"].includes(screen);
  if (inRoom) {
    void c.selectChannel("general").then(() => {
      c.waddles.members.value = roomMembers;
      c.waddles.memberLoadState.value = "ready";
      c.messaging.roomPresence.value = Object.fromEntries(
        PEOPLE.map(([local, , show]) => [local, show === "available" ? "online" : show]),
      ) as never;
      if (screen.startsWith("members-manage")) c.ui.showMembers.value = true;
      if (screen === "members-room") c.openMembers();
      if (screen === "dialog-edit") c.ui.showEditChannel.value = true;
    });
  }

  if (screen === "members" || screen === "members-empty") c.openMembers();
  if (screen.startsWith("rooms")) c.openRooms();
  if (screen === "dialog-create") { c.openRooms(); c.ui.showCreateChannel.value = true; }
  if (screen === "dialog-waddle") { c.openRooms(); c.ui.showWaddleSettings.value = true; }
  if (screen === "dialog-newdm") c.ui.showNewDm.value = true;
  if (screen === "dialog-newgroup") c.ui.showNewGroupDm.value = true;
}
