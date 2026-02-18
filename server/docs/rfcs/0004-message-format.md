# RFC-0004: Rich Message Format

## Summary

Messages in Waddle Social use XMPP stanzas with standard XEPs for rich formatting, attachments, reactions, and threading.

## Motivation

Modern communication requires:

- Text formatting for emphasis and structure
- Media attachments (images, files, videos)
- Link previews for context
- Reactions for lightweight responses
- Threads for focused discussions

## Detailed Design

### XMPP Message Structure

Messages use standard XMPP `<message>` stanzas with extensions:

```xml
<message from='alice@waddle.social/device1'
         to='general@muc.penguin-club.waddle.social'
         type='groupchat'
         id='msg-uuid-here'>
  <body>Hey @bob, check out this **awesome** feature!</body>
  <html xmlns='http://jabber.org/protocol/xhtml-im'>
    <body xmlns='http://www.w3.org/1999/xhtml'>
      Hey <span class='mention'>@bob</span>, check out this <strong>awesome</strong> feature!
    </body>
  </html>
  <reference xmlns='urn:xmpp:reference:0' type='mention' uri='xmpp:bob@waddle.social' begin='4' end='8'/>
  <stanza-id xmlns='urn:xmpp:sid:0' id='archive-id' by='general@muc.penguin-club.waddle.social'/>
</message>
```

### Text Formatting

Uses XHTML-IM (XEP-0071) for rich text:

| Syntax | XHTML Element |
|--------|---------------|
| `**bold**` | `<strong>` |
| `*italic*` | `<em>` |
| `~~strike~~` | `<del>` |
| `` `code` `` | `<code>` |
| Code blocks | `<pre><code>` |
| `> quote` | `<blockquote>` |
| `[link](url)` | `<a href>` |

Clients render Markdown → XHTML-IM on send, and can display either format.

### Attachments

Uses XEP-0363 (HTTP File Upload) and XEP-0385 (SIMS):

```xml
<message to='general@muc.penguin-club.waddle.social' type='groupchat'>
  <body>Check out this screenshot</body>
  <reference xmlns='urn:xmpp:reference:0' type='data'>
    <media-sharing xmlns='urn:xmpp:sims:1'>
      <file xmlns='urn:xmpp:jingle:apps:file-transfer:5'>
        <media-type>image/png</media-type>
        <name>screenshot.png</name>
        <size>245678</size>
        <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>base64hash</hash>
        <thumbnail xmlns='urn:xmpp:thumbs:1' uri='cid:thumb@waddle.social' media-type='image/png' width='128' height='72'/>
      </file>
      <sources>
        <reference xmlns='urn:xmpp:reference:0' type='data' uri='https://cdn.waddle.social/uploads/abc/screenshot.png'/>
      </sources>
    </media-sharing>
  </reference>
</message>
```

Supported types:

- Images: JPEG, PNG, GIF, WebP
- Video: MP4, WebM
- Audio: MP3, OGG, WAV
- Documents: PDF, TXT, MD
- Archives: ZIP (preview disabled)

### Link Previews (Embeds)

Clients or the server can add OGP metadata via XEP-0385:

```xml
<reference xmlns='urn:xmpp:reference:0' type='data' uri='https://github.com/waddle-social/wa'>
  <meta xmlns='urn:xmpp:ogp:0'>
    <og:title>waddle-social/wa</og:title>
    <og:description>Open source communication platform</og:description>
    <og:image>https://opengraph.githubassets.com/...</og:image>
    <og:site_name>GitHub</og:site_name>
  </meta>
</reference>
```

### Mentions

Uses XEP-0372 (References):

```xml
<message to='general@muc.penguin-club.waddle.social' type='groupchat'>
  <body>Hey @bob, thoughts?</body>
  <reference xmlns='urn:xmpp:reference:0' type='mention' uri='xmpp:bob@waddle.social' begin='4' end='8'/>
</message>
```

Mention types:

- **User**: `uri='xmpp:user@waddle.social'`
- **Channel**: `uri='xmpp:channel@muc.waddle.waddle.social'`
- **Everyone**: Custom namespace `<mention xmlns='urn:waddle:mention:0' type='everyone'/>`

### Reactions

Uses XEP-0444 (Message Reactions):

```xml
<message to='general@muc.penguin-club.waddle.social' type='groupchat'>
  <reactions id='original-message-id' xmlns='urn:xmpp:reactions:0'>
    <reaction>👍</reaction>
  </reactions>
</message>
```

To remove a reaction, send an empty `<reactions>` element.

### Replies & Threads

Uses XEP-0461 (Message Replies):

```xml
<message to='general@muc.penguin-club.waddle.social' type='groupchat'>
  <body>Great point!</body>
  <reply xmlns='urn:xmpp:reply:0' to='original-message-id'/>
  <thread>thread-id</thread>
</message>
```

### Message Editing

Uses XEP-0308 (Last Message Correction):

```xml
<message to='general@muc.penguin-club.waddle.social' type='groupchat'>
  <body>Fixed typo</body>
  <replace id='original-message-id' xmlns='urn:xmpp:message-correct:0'/>
</message>
```

### Message Retraction

Uses XEP-0424 (Message Retraction):

```xml
<message to='general@muc.penguin-club.waddle.social' type='groupchat'>
  <retract id='message-to-delete' xmlns='urn:xmpp:message-retract:1'/>
  <fallback xmlns='urn:xmpp:fallback:0'/>
  <body>This message was deleted</body>
</message>
```

### Message Limits

| Resource | Limit |
|----------|-------|
| Body length | 4000 characters |
| Attachments per message | 10 |
| Embeds per message | 5 |
| Reactions per message | 50 unique |
| Mentions per message | 50 |

### End-to-End Encryption

Uses XEP-0384 (OMEMO):

```xml
<message to='bob@waddle.social' type='chat'>
  <encrypted xmlns='eu.siacs.conversations.axolotl'>
    <header sid='device-id'>
      <key rid='recipient-device-id'>base64-encrypted-key</key>
      <iv>base64-iv</iv>
    </header>
    <payload>base64-encrypted-content</payload>
  </encrypted>
  <store xmlns='urn:xmpp:hints'/>
</message>
```

## Message Archive

Messages are archived via XEP-0313 (MAM) and can be queried:

```xml
<iq type='set' id='q1'>
  <query xmlns='urn:xmpp:mam:2'>
    <x xmlns='jabber:x:data' type='submit'>
      <field var='FORM_TYPE'><value>urn:xmpp:mam:2</value></field>
    </x>
    <set xmlns='http://jabber.org/protocol/rsm'>
      <max>50</max>
      <before/>
    </set>
  </query>
</iq>
```

## Related

- [RFC-0002: Channels](./0002-channels.md)
- [RFC-0005: Ephemeral Content](./0005-ephemeral-content.md)
- [ADR-0006: XMPP Protocol](../adrs/0006-xmpp-protocol.md)
- [Spec: XMPP Integration](../specs/xmpp-integration.md)
