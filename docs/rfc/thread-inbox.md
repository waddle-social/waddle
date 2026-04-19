# Thread Inbox Extension

**Namespace:** `urn:xmpp:inbox:0` (extends XEP-0430 Inbox)
**Status:** Custom extension (Waddle-specific)
**Dependencies:** XEP-0430 (Inbox), RFC 6121 (`<thread/>`), XEP-0508 (Forums)

## Overview

Extends the XEP-0430 inbox with thread-level granularity. A thread is a
sub-conversation within a MUC room, identified by an RFC 6121 `<thread/>`
element. Thread inbox entries share the same `<conversation>` element,
storage, and push mechanism as channel-level entries — differentiated by
the presence of a `thread` attribute.

## Wire Format

### Thread-Level Conversation Entry

The existing `<conversation>` element gains optional thread attributes:

```xml
<conversation xmlns='urn:xmpp:inbox:0'
              partner='general@muc.waddle.social'
              kind='muc'
              thread='thread-42'
              thread-title='Getting Started'
              reply-count='7'
              author='alice'
              last-stanza-id='sid-99'
              last-updated='1713500000'
              unread='2'>
  <preview>latest reply text</preview>
</conversation>
```

| Attribute      | Type    | Required | Description                                                |
|----------------|---------|----------|------------------------------------------------------------|
| `thread`       | string  | No       | RFC 6121 thread ID. Absent for channel-level entries.      |
| `thread-title` | string  | No       | XEP-0508 thread title or first message preview.            |
| `reply-count`  | integer | No       | Total replies in the thread (server-maintained).           |
| `author`       | string  | No       | Nick of the thread starter.                                |

### Query: Thread Entries for a Room

To fetch thread-level inbox entries for a specific room, add `room` and
`threads='true'` attributes to the query:

```xml
<iq type='get' id='ti-1'>
  <query xmlns='urn:xmpp:inbox:0'
         room='general@muc.waddle.social'
         threads='true'/>
</iq>
```

Response contains only thread-level entries for that room:

```xml
<iq type='result' id='ti-1'>
  <query xmlns='urn:xmpp:inbox:0' total-unread='3'>
    <conversation partner='general@muc.waddle.social' kind='muc'
                  thread='thread-42' thread-title='Getting Started'
                  reply-count='7' author='alice'
                  last-stanza-id='sid-99' last-updated='1713500000'
                  unread='2'/>
    <conversation partner='general@muc.waddle.social' kind='muc'
                  thread='thread-55'
                  last-stanza-id='sid-104' last-updated='1713490000'
                  unread='1' reply-count='3'/>
  </query>
</iq>
```

The existing query without `room`/`threads` continues to return only
channel-level entries (backwards compatible).

### Mark Thread as Read

Add an optional `thread` attribute to `<mark-read>`:

```xml
<iq type='set' id='ti-2'>
  <mark-read xmlns='urn:xmpp:inbox:0'
             partner='general@muc.waddle.social'
             thread='thread-42'/>
</iq>
```

Without `thread`, the existing channel-level mark-read behaviour is
unchanged.

### Server Push

When a MUC message carrying a `<thread/>` element is received, the server
pushes both the channel-level and thread-level updated entries via
headline messages:

```xml
<!-- Channel-level push (existing behaviour) -->
<message type='headline' to='user@waddle.social'>
  <conversation xmlns='urn:xmpp:inbox:0'
                partner='general@muc.waddle.social' kind='muc'
                last-stanza-id='sid-105' last-updated='1713501000'
                unread='6'/>
</message>

<!-- Thread-level push (new) -->
<message type='headline' to='user@waddle.social'>
  <conversation xmlns='urn:xmpp:inbox:0'
                partner='general@muc.waddle.social' kind='muc'
                thread='thread-42' thread-title='Getting Started'
                reply-count='8' author='alice'
                last-stanza-id='sid-105' last-updated='1713501000'
                unread='3'/>
</message>
```

## Storage

The inbox storage composite key is `(user_jid, partner_jid, thread_id)`.
For channel-level entries, `thread_id` is empty. Additional columns:

- `thread_title TEXT` — nullable
- `reply_count INTEGER DEFAULT 0`
- `author TEXT` — nullable

## Thread Title Resolution

When a MUC message arrives with a `<thread/>`:

1. Check for XEP-0508 `<thread-create title='...'/>` — use as title
2. Otherwise, use the first message body as a preview title
3. Once set, the title is preserved across subsequent replies

## Scope

- Applies to **all channel types** (text and forum)
- Thread entries appear for any message carrying an RFC 6121 `<thread/>` element
- Server pushes updates in real-time via headline messages
- Clients display active threads under channel names in the sidebar
