# Planning an event in Waddle

Open **Events** on Android or the web to create an event planning space. You can
start with a title while dates, accommodation and venues are still undecided.
For example, create “Waddle maintainers — London, February 2027”, choose
`Europe/London` as its time zone, then add dates when everyone has agreed.

## Invite the group

Event planning spaces are private rooms. Invite members and appoint
co-organisers through membership management; co-organisers can update the event
and manage shared planning items. An invitation grants access, while an RSVP
records whether someone intends to attend. These are separate actions.

An invited person can open the event's room address from Events and save it to
find it again on another device. Event bookmarks synchronise through the account.
Use the conversation for messages, replies, mentions, attachments and photos, and
adjust its notification settings to suit your involvement.

## Build the plan

- **Overview:** description, location, time zone and proposed, confirmed or
  cancelled status. Pin important items here.
- **Saved links:** accommodation, venues, restaurants and transport, with notes
  and categories. Save file and photo links alongside other resources; upload
  attachments through the conversation.
- **Itinerary:** timed activities or undated ideas, with a location, notes, links
  and a responsible person. Location links open a map.
- **Tasks and decisions:** record outstanding actions, deadlines, assignees and
  agreements. Assignees can manage their assigned tasks.
- **Polls:** collect choices, change your vote and close voting when settled.
- **People and travel:** record attendance, arrivals and departures. Travel
  information is shared with the invited group.
- **Expenses:** record amounts and currencies with the people sharing a cost.
  Amounts use the currency's minor units to preserve exact costs.

Share an item into the event conversation to discuss it. Refresh the planning
view to pick up other people's changes; the web also refreshes while visible.
Each item is saved separately so adding a hotel link does not replace someone
else's itinerary entry. Editing the same item concurrently uses the latest
accepted write.

## Calendars and privacy

Timed activities retain their time zone and UTC instant. Event planning supports
unknown dates; calendar exports include only scheduled events and itinerary
entries. Export an `.ics` file to import into your calendar.

The existing **community calendar is public**, including its calendar feed.
An organiser can attach a private planning space to a public calendar event;
the public listing does not grant access to that space. Keep private travel
details in the planning space. Exporting a private calendar file shares the
information in that file with whoever receives it.

Removing someone's event membership revokes future planning requests. A deleted
room recreated at the same address gets a fresh planning data scope. Planning
payloads are not projected into public calendar feeds or community activity.

The wire contract and server permission model are documented in
[the event planning specification](../../server/docs/specs/event-planning.md).

## Preview

The development preview includes a London maintainers meetup in February 2027.
It uses the production planning component with local fixture storage, so it can
be explored without an account at `/_preview?screen=event-planning`.

[Desktop preview](../screenshots/event-planning/desktop.png) ·
[Mobile preview](../screenshots/event-planning/mobile.png)
