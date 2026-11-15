# Event planning workspaces

Event planning data is private to the persistent members of an event's MUC room.
The public community xCal calendar and its ICS feed are a separate projection:
private event metadata, travel details and planning objects must never be published
to that calendar. The existing room conversation supplies replies, attachments,
mentions and per-room notification controls.

## Transport and access

Use XEP-0060 IQs addressed to the event room's **bare JID**, with node
`urn:waddle:event-planning:0`. Only item retrieval, publication and retraction are
supported. Subscription, configuration and node-administration requests are denied;
clients refresh the workspace explicitly. Every request checks current MUC
membership, including for offline members. A nonmember occupant of an open room
cannot access planning objects. Revoking membership revokes subsequent reads and
writes without a separate PubSub affiliation synchronization step.

Objects persist independently through the existing PubSub storage. Internally,
the node is scoped to the room lifecycle, so destroying and recreating the same
room address cannot expose the previous event's planning data, including when a
request from the old lifecycle finishes late. The service
stamps both the payload author and PubSub publisher from the authenticated sender.
Organizer edits preserve the original authenticated publisher rather than
transferring authorship.
Members can replace or retract their own objects. Current task assignees can
manage their assigned tasks while preserving attribution and assignment. Owners
and administrators can moderate shared objects. The overview uses reserved ID `event` and is editable only
by owners and administrators. Travel records, votes and attendance remain authored
by the person publishing them.

No planning payload is broadcast through PubSub subscriptions: this avoids stale
subscriptions retaining access after a person leaves the event. The backing node
also uses whitelist access and disables last-item and payload notifications.

## Payload

`waddle_xmpp_core::event_planning::PlanningItem` is the shared typed contract. The
payload root is `<item xmlns='urn:waddle:event-planning:0' id='…' kind='…'>`.
The server adds `author='person@example.com'`.

Supported kinds are `event`, `link`, `task`, `itinerary`, `decision`, `travel`,
`expense`, `poll`, `vote`, and `attendance`. Ordinary IDs are at most 128 bytes and
cannot contain a colon. Each payload has a `title`; votes and attendance may use an
empty title. Optional scalar child elements are:

- `description`, `category`, `location`, `url`, `assignee`;
- `timezone` (IANA identifier), `starts-at` and `ends-at` (RFC 3339 instants);
- `completed` and `pinned` (literal `true` or `false`);
- `amount-minor` (non-negative integer for expenses), `currency` (uppercase ISO code);
- `subject` (referenced poll ID), `attendance`, and `status`.

Repeated `participant` children contain bare JIDs. Polls contain at least two
`<option id='stable-id'>Human-readable choice</option>` children; votes contain
`<selection>stable-id</selection>` children. Scalar fields cannot repeat.

An unscheduled event omits its timestamps. If an end is supplied, a start is
required and the end must be later. Timezone changes alter presentation, not the
stored instant. Links accept HTTP(S) URLs without embedded credentials. Expense
amounts use minor units to avoid floating-point rounding and stay within the
JavaScript safe-integer range so native-created expenses remain readable on web.

`status` is `proposed`, `confirmed`, or `cancelled`. `attendance` is `invited`,
`interested`, `going`, or `declined`. Attendance IDs are
`attendance:{authenticated-bare-jid}`. Vote IDs are
`vote:{poll-id}:{authenticated-bare-jid}`. A person cannot publish another person's
response. Poll choices are checked against the referenced poll; `completed=true`
closes a poll to voting.
