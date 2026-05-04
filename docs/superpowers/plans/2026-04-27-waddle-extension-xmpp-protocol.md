# Waddle Extension Framework XMPP/Protocol Implementation Spec

**Status:** implementation spec, no code in this plan
**Scope:** XMPP protocol slice only
**Control plane:** `extensions.<domain>`
**Framework namespace:** `urn:waddle:extension:1`

## Goal

Build the Waddle extension/plugin protocol as an XMPP-native framework that a
junior developer can implement without inventing wire shapes. The first
release supports these three sample plugins:

- Link Board
- Standard AI Chatbot
- Decision Polls

This spec defines the protocol, digest-pinned WASM artifact model,
declarative client surfaces, permission grants, lifecycle states, routes, and
test expectations for the first implementation slice. It intentionally does
not define billing, marketplace ranking, or provider-specific AI code.

HTTP(S) is allowed only for immutable artifacts and explicitly granted
server-side enrichment fetches such as OpenGraph metadata. All mutable control,
launch, configuration, state, and interaction flows go through XMPP to
`extensions.<domain>`.

## Hard Rules

- The only Waddle framework namespace is `urn:waddle:extension:1`.
- Plugin payload namespaces must be Waddle-owned and must start with
  `urn:waddle:`.
- Official XEP namespaces may appear only when the wire shape is exactly that
  XEP's shape.
- User clients never receive or submit user/provider secrets. No XEP-0050 form
  may request API keys, OAuth refresh tokens, webhook secrets, or provider
  credentials.
- Extensions never render in iframes, WebViews, plugin DOM, plugin JS, or
  plugin CSS. Clients render server-validated declarative view descriptors and
  immutable media artifacts only.
- WASM components are executed only by the server extension runtime after an
  admin grants an installed digest. User clients never download or run plugin
  WASM.
- WASM artifacts must be referenced by OCI digest, not tag. A mutable tag,
  missing digest, or digest mismatch is an install failure.
- Client stanzas must not publish arbitrary extension XML into chat. Server-side
  extension enrichment is the only path that adds framework enrichment payloads
  to messages.
- Message enrichment is one framework feature. No plugin may add direct
  top-level message payloads outside `<extensions xmlns='urn:waddle:extension:1'>`.
- Extension actions, bot replies, board updates, AI requests, and poll votes are
  XMPP-native operations. Do not add REST, GraphQL, webhook, or browser
  postMessage control paths.

## XEP Baseline

Review and follow the local XEP sources before implementing:

- XEP-0030 Service Discovery:
  `http://jabber.org/protocol/disco#info`,
  `http://jabber.org/protocol/disco#items`
- XEP-0004 Data Forms: `jabber:x:data`
- XEP-0050 Ad-Hoc Commands: `http://jabber.org/protocol/commands`
- XEP-0060 Publish-Subscribe:
  `http://jabber.org/protocol/pubsub`,
  `http://jabber.org/protocol/pubsub#owner`,
  `http://jabber.org/protocol/pubsub#event`,
  `http://jabber.org/protocol/pubsub#errors`
- XEP-0163 PEP only for account PEP. The extension control plane is a PubSub
  component, not a PEP node on a user bare JID.
- XEP-0280 Message Carbons: `urn:xmpp:carbons:2`
- XEP-0297 Stanza Forwarding: `urn:xmpp:forward:0`
- XEP-0313 MAM: `urn:xmpp:mam:2`
- XEP-0334 Message Processing Hints: `urn:xmpp:hints`
- XEP-0359 Stanza IDs: `urn:xmpp:sid:0`
- XEP-0372 References: `urn:xmpp:reference:0`
- XEP-0461 Replies: `urn:xmpp:reply:0`
- XEP-0513 Mentions: `urn:xmpp:mentions:0`

Implementation notes from the local XEP review:

- XEP-0030 node semantics belong to the using protocol. Waddle uses nodes only
  where the extension component, command list, and PubSub nodes need them.
- XEP-0050 command discovery uses disco items at the fixed
  `http://jabber.org/protocol/commands` node, and each returned item node is
  the command node a client executes.
- XEP-0060 PubSub nodes are addressed as JID plus node on
  `extensions.<domain>`, and node discovery/configuration must use the XEP
  namespaces and Data Forms `FORM_TYPE` values exactly.
- XEP-0004 `jabber:x:data` is advertised because commands and PubSub forms
  depend on it; extension-specific meaning stays in Waddle field names and
  Waddle namespaces.

## Namespaces

Use these exact Waddle namespaces:

| Use | Namespace |
| --- | --- |
| Framework envelope, manifests, launch metadata, command form `FORM_TYPE` | `urn:waddle:extension:1` |
| Link Board payloads | `urn:waddle:link-board:1` |
| Standard AI Chatbot payloads | `urn:waddle:ai-chatbot:1` |
| Decision Polls payloads | `urn:waddle:decision-polls:1` |

Do not introduce `urn:waddle:github:*`, `urn:waddle:bot:*`,
`urn:waddle:enrichment:*`, `urn:xmpp:*`, `jabber:*`, or
`http://jabber.org/*` namespaces for Waddle-specific semantics.

## Negotiation Model

Negotiation is XMPP-native and happens before any extension UI, action, or
runtime invocation is trusted.

Client negotiation:

1. Discover `extensions.<domain>` from the account domain with XEP-0030
   `disco#items`.
2. Query `extensions.<domain>` with XEP-0030 `disco#info`.
3. Proceed only if the response advertises `urn:waddle:extension:1`,
   XEP-0050 commands, XEP-0060 PubSub, and XEP-0004 data forms.
4. Discover command nodes by querying the fixed
   `http://jabber.org/protocol/commands` node.
5. Retrieve installed extensions and visible plugin state through XEP-0060
   nodes for the Waddle/room the user can access.
6. Render only declarative descriptors carried in installed manifests,
   PubSub items, or server-authored message enrichment.

Server/runtime negotiation:

1. The admin install command submits a plugin id, payload namespace,
   requested capabilities, requested permissions, and an OCI artifact digest.
2. The server pulls the OCI manifest by digest, verifies every descriptor
   digest, verifies the WASM component digest, and rejects tag-only references.
3. The server loads the component's WIT version and requires
   `waddle:extension@1.0.0#world:waddle-extension`; incompatible major versions fail install.
4. The server compares requested capabilities and permissions with the
   component manifest. The installed grant is the intersection approved by the
   admin, never the component's self-declared maximum.
5. The server publishes the installed grant to the Waddle installation node and
   writes an audit item before enabling runtime triggers.

Compatibility rules:

- `urn:waddle:extension:1` is the only protocol version in this slice.
- A future `urn:waddle:extension:2` must be negotiated as a separate feature and
  must not change the wire shape for version 1 clients.
- If a client does not understand an enrichment payload namespace, it must hide
  that plugin surface and still show the original chat message body.
- If a client understands the framework namespace but not a launch action, it
  must not invoke that action.

## Disco Entities

Assume the primary XMPP domain is `example.com`. The extension component JID is
`extensions.example.com`.

### Server Disco Items

A disco items query to `example.com` with no node must include:

```xml
<item jid='extensions.example.com' name='Waddle Extensions'/>
```

Keep existing MUC, upload, and Spaces items unchanged.

### `extensions.example.com` Disco Info

For:

```xml
<iq type='get' to='extensions.example.com' id='disco-ext-1'>
  <query xmlns='http://jabber.org/protocol/disco#info'/>
</iq>
```

return:

```xml
<iq type='result' from='extensions.example.com' id='disco-ext-1'>
  <query xmlns='http://jabber.org/protocol/disco#info'>
    <identity category='pubsub' type='service' name='Waddle Extensions'/>
    <feature var='http://jabber.org/protocol/disco#info'/>
    <feature var='http://jabber.org/protocol/disco#items'/>
    <feature var='http://jabber.org/protocol/commands'/>
    <feature var='jabber:x:data'/>
    <feature var='http://jabber.org/protocol/pubsub'/>
    <feature var='http://jabber.org/protocol/pubsub#access-open'/>
    <feature var='http://jabber.org/protocol/pubsub#access-whitelist'/>
    <feature var='http://jabber.org/protocol/pubsub#config-node'/>
    <feature var='http://jabber.org/protocol/pubsub#create-and-configure'/>
    <feature var='http://jabber.org/protocol/pubsub#create-nodes'/>
    <feature var='http://jabber.org/protocol/pubsub#delete-nodes'/>
    <feature var='http://jabber.org/protocol/pubsub#persistent-items'/>
    <feature var='http://jabber.org/protocol/pubsub#publish'/>
    <feature var='http://jabber.org/protocol/pubsub#publisher-affiliation'/>
    <feature var='http://jabber.org/protocol/pubsub#retrieve-items'/>
    <feature var='http://jabber.org/protocol/pubsub#retract-items'/>
    <feature var='http://jabber.org/protocol/pubsub#subscribe'/>
    <feature var='urn:waddle:extension:1'/>
  </query>
</iq>
```

Do not advertise XEP-0313/MAM on `extensions.example.com` unless an extension
archive is implemented. Extension state is PubSub state; chat history remains
in the normal user/MUC MAM archives.

### Commands List Disco

For disco items to node `http://jabber.org/protocol/commands`:

```xml
<iq type='get' to='extensions.example.com' id='cmd-items-1'>
  <query xmlns='http://jabber.org/protocol/disco#items'
         node='http://jabber.org/protocol/commands'/>
</iq>
```

return these command items, filtered by requester authorization:

```xml
<iq type='result' from='extensions.example.com' id='cmd-items-1'>
  <query xmlns='http://jabber.org/protocol/disco#items'
         node='http://jabber.org/protocol/commands'>
    <item jid='extensions.example.com'
          node='urn:waddle:extension:1:admin:list'
          name='List Waddle Extensions'/>
    <item jid='extensions.example.com'
          node='urn:waddle:extension:1:admin:install'
          name='Install Waddle Extension'/>
    <item jid='extensions.example.com'
          node='urn:waddle:extension:1:admin:configure'
          name='Configure Waddle Extension'/>
    <item jid='extensions.example.com'
          node='urn:waddle:extension:1:admin:set-enabled'
          name='Enable or Disable Waddle Extension'/>
    <item jid='extensions.example.com'
          node='urn:waddle:extension:1:admin:uninstall'
          name='Uninstall Waddle Extension'/>
    <item jid='extensions.example.com'
          node='urn:waddle:extension:1:invoke'
          name='Run Waddle Extension Action'/>
  </query>
</iq>
```

Non-admin Waddle members get only `admin:list` and `invoke`, and only for
Waddles/rooms they can access. Unauthenticated users get an empty command list.

### Command Node Disco Info

Every command node above must answer disco info with:

```xml
<identity category='automation' type='command-node' name='...'/>
<feature var='http://jabber.org/protocol/commands'/>
<feature var='jabber:x:data'/>
<feature var='urn:waddle:extension:1'/>
```

The fixed command-list node `http://jabber.org/protocol/commands` must answer
with identity `category='automation' type='command-list'`.

### PubSub Node Disco Info

Every extension PubSub leaf node must answer disco info with:

```xml
<identity category='pubsub' type='leaf' name='...'/>
<feature var='http://jabber.org/protocol/pubsub'/>
<feature var='http://jabber.org/protocol/pubsub#retrieve-items'/>
```

If metadata is returned, use XEP-0060 metadata form type exactly:

```xml
<x xmlns='jabber:x:data' type='result'>
  <field var='FORM_TYPE' type='hidden'>
    <value>http://jabber.org/protocol/pubsub#meta-data</value>
  </field>
  <field var='pubsub#type' type='text-single'>
    <value>urn:waddle:extension:1</value>
  </field>
  <field var='pubsub#title' type='text-single'>
    <value>Installed Waddle Extensions</value>
  </field>
  <field var='pubsub#access_model' type='list-single'>
    <value>whitelist</value>
  </field>
  <field var='pubsub#publish_model' type='list-single'>
    <value>publishers</value>
  </field>
  <field var='pubsub#max_items' type='text-single'>
    <value>256</value>
  </field>
</x>
```

Use the plugin payload namespace as `pubsub#type` on plugin-specific data
nodes, for example `urn:waddle:decision-polls:1` on poll nodes.

## PubSub Node Model

All mutable extension state is PubSub under `extensions.<domain>`. Do not add an
HTTP control API for install/config/invoke/state. Node names are stable strings;
replace `{waddle-id}`, `{plugin-id}`, `{game-id}`, or `{poll-id}` with the
canonical server IDs, not display names.

Use XEP-0060 node configuration fields exactly:

- `pubsub#access_model`: `open` or `whitelist`
- `pubsub#publish_model`: `publishers`
- `pubsub#persist_items`: `1` or `0`
- `pubsub#deliver_payloads`: `1`
- `pubsub#send_last_published_item`: `on_sub` unless noted otherwise
- `pubsub#max_items`: decimal integer or `max`
- `pubsub#notify_retract`: `1`
- `pubsub#notify_delete`: `1`
- `pubsub#type`: payload namespace
- `pubsub#title`: human-readable node title
- `pubsub#description`: optional human-readable node description

Example owner config submit for a member-visible extension node:

```xml
<iq type='set' to='extensions.example.com' id='cfg-node-1'>
  <pubsub xmlns='http://jabber.org/protocol/pubsub#owner'>
    <configure node='urn:waddle:extension:1:waddle:waddle-123:installations'>
      <x xmlns='jabber:x:data' type='submit'>
        <field var='FORM_TYPE' type='hidden'>
          <value>http://jabber.org/protocol/pubsub#node_config</value>
        </field>
        <field var='pubsub#access_model'><value>whitelist</value></field>
        <field var='pubsub#publish_model'><value>publishers</value></field>
        <field var='pubsub#persist_items'><value>1</value></field>
        <field var='pubsub#deliver_payloads'><value>1</value></field>
        <field var='pubsub#send_last_published_item'><value>on_sub</value></field>
        <field var='pubsub#max_items'><value>256</value></field>
        <field var='pubsub#notify_retract'><value>1</value></field>
        <field var='pubsub#notify_delete'><value>1</value></field>
        <field var='pubsub#type'><value>urn:waddle:extension:1</value></field>
        <field var='pubsub#title'><value>Installed Waddle Extensions</value></field>
      </x>
    </configure>
  </pubsub>
</iq>
```

Do not use `presence` or `roster` access models for Waddle member data in this
slice. Waddle membership is not the same thing as XMPP roster/presence sharing.
Use `whitelist` and maintain affiliations/subscriptions from Waddle membership.

### Framework Nodes

| Node | Access | Publish | Max | Type | Purpose |
| --- | --- | --- | --- | --- | --- |
| `urn:waddle:extension:1:catalog` | `open` | `publishers` | `max` | `urn:waddle:extension:1` | Public catalog of installable immutable artifacts. |
| `urn:waddle:extension:1:waddle:{waddle-id}:installations` | `whitelist` | `publishers` | `256` | `urn:waddle:extension:1` | Installed plugins and grants for one Waddle. |
| `urn:waddle:extension:1:waddle:{waddle-id}:audit` | `whitelist` | `publishers` | `max` | `urn:waddle:extension:1` | Install/config/enable/invoke audit events. |
| `urn:waddle:extension:1:waddle:{waddle-id}:launches` | `whitelist` | `publishers` | `1000` | `urn:waddle:extension:1` | Recent launch descriptors that need PubSub notifications. |
| `urn:waddle:extension:1:waddle:{waddle-id}:plugin:{plugin-id}:config` | `whitelist` | `publishers` | `1` | `urn:waddle:extension:1` | Non-secret plugin config. Admin whitelist only. |
| `urn:waddle:extension:1:waddle:{waddle-id}:plugin:{plugin-id}:state` | `whitelist` | `publishers` | `max` | plugin payload namespace | Plugin-owned state visible to Waddle members unless the plugin-specific table narrows it. |

Framework node item payloads use:

```xml
<extension xmlns='urn:waddle:extension:1'
           id='link-board'
           name='Link Board'
           version='1.0.0'
           payload-ns='urn:waddle:link-board:1'
           enabled='true'>
  <artifact oci='oci://registry.example.com/waddle/extensions/link-board@sha256:1111222233334444555566667777888899990000aaaabbbbccccddddeeeeffff'
            wasm-digest='sha256:aaaabbbbccccddddeeeeffff1111222233334444555566667777888899990000'/>
  <capability name='message.enrich'/>
  <capability name='pubsub.read'/>
  <capability name='pubsub.publish'/>
  <capability name='commands'/>
  <capability name='launch'/>
  <permission name='net.fetch.opengraph' scope='message-link-hosts'/>
  <permission name='pubsub.publish' scope='urn:waddle:link-board:1:waddle:{waddle-id}:*'/>
  <bot jid='link-board@extensions.example.com'/>
</extension>
```

The artifact reference must be an OCI reference with an `@sha256:` digest and a
separate WASM component digest. The server must reject catalog or installation
payloads with mutable artifact references, missing hashes, non-Waddle payload
namespaces, unknown capabilities, or permissions not declared by the component.

## WASM OCI Artifact Model

Use OCI as the distribution format and digest pinning as the trust boundary.
The extension runtime does not fetch arbitrary URLs supplied by clients.

Required install fields:

| Field | Example | Rule |
| --- | --- | --- |
| `waddle#artifact_oci` | `oci://registry.example.com/waddle/extensions/link-board@sha256:...` | Must include `@sha256:`. Tags such as `:latest` are rejected even when paired with a digest elsewhere. |
| `waddle#wasm_digest` | `sha256:...` | Must match the WASM component layer or artifact descriptor digest. |
| `waddle#wit_world` | `waddle:extension@1.0.0#world:waddle-extension` | Must match the runtime world supported by the server. |
| `waddle#manifest_digest` | `sha256:...` | Must match the extension manifest item stored in the OCI artifact. |

OCI pull and verification steps:

1. Resolve the OCI reference by digest only. Do not resolve tags.
2. Fetch the OCI manifest and verify the manifest digest matches the reference.
3. Verify config, layer, WIT, manifest, and static asset descriptor digests
   before writing anything to the local cache.
4. Require exactly one WASM component layer with media type
   `application/wasm` or `application/vnd.wasm.component`.
5. Require the component manifest to declare plugin id, payload namespace,
   WIT world, capabilities, permissions, routes, and sample fixtures.
6. Cache verified artifacts under `plugin-id/digest/` and make the cache
   content-addressed and immutable.
7. Instantiate WASM with no ambient network, filesystem, clock, or environment
   access except host functions explicitly listed by granted permissions.

Static assets:

- Static assets from the verified OCI artifact may be served at
  `/_extensions/{plugin-id}/{digest}/{path}`.
- Static routes are `GET` and `HEAD` only, immutable, cacheable, and must add a
  content hash or digest in their path.
- Static assets may include icons, thumbnails, generated image renders, and
  declarative view schemas. They must not include executable client JavaScript,
  iframe HTML, plugin CSS, or secrets.
- The route handler must validate `{digest}` against the installed grant before
  serving the file to a member.

### Capability Names

Use these exact capability names in framework manifest payloads:

| Capability | Meaning |
| --- | --- |
| `message.enrich` | Server may call the plugin during message acceptance and attach framework enrichment. |
| `message.observe` | Plugin may observe accepted message metadata for trigger decisions, but may not mutate the message. |
| `commands` | Plugin exposes actions through XEP-0050 command forms. |
| `launch` | Plugin may create launch descriptors that clients invoke through XEP-0050. |
| `bot.respond` | Plugin may send XMPP chat/groupchat messages from its bot JID. |
| `pubsub.read` | Plugin may read only declared PubSub nodes. |
| `pubsub.publish` | Plugin may publish only declared PubSub nodes. |
| `artifact.reference` | Plugin may reference immutable HTTP(S) artifacts by URI plus hash. |
| `ui.declarative` | Plugin may expose declarative client surfaces rendered by Waddle clients. |
| `channels.read` | Plugin may list approved channels and spaces. |
| `members.read` | Plugin may list approved room members. |
| `presence.read` | Plugin may read approved room/member presence. |
| `message.send` | Plugin may send approved MUC, DM, or member messages as its bot identity. |
| `mam.query` | Plugin may query bounded XEP-0313 history for approved MUC or DM context. |
| `roster.read` | Plugin may read approved roster entries. |
| `bot.presence` | Plugin may register bot member/presence surfaces. |

### Permission Grants

Capabilities say what family of behavior a plugin can perform. Permissions say
where and under what limits it can perform that behavior. Installation stores
the approved permission set in the `installations` item; runtime checks use
that grant on every trigger.

Use these permission names:

| Permission | Scope | Rule |
| --- | --- | --- |
| `room.read.metadata` | Waddle/room IDs | Read room identity, display name, and membership class only. |
| `message.observe` | Waddle/room IDs | See accepted message metadata and body for trigger matching. |
| `message.enrich` | Waddle/room IDs | Add framework enrichment during server message acceptance. |
| `mam.read.context` | Waddle/room IDs and max message count | Read bounded recent MAM context for AI/bot replies. |
| `bot.send.message` | Waddle/room IDs | Send chat/groupchat messages from the plugin bot JID. |
| `pubsub.read` | Exact node or prefix pattern | Read only matching nodes after requester membership checks. |
| `pubsub.publish` | Exact node or prefix pattern | Publish only matching typed payloads as server/plugin publisher. |
| `net.fetch.opengraph` | Host allowlist and byte/time limits | Server-side fetch of OpenGraph metadata for URLs already present in accepted messages. |
| `artifact.write` | Artifact class and max bytes | Write immutable generated artifacts. |
| `extension.provider.config` | Extension-owned provider settings | Configure provider behavior inside the extension. The server does not expose an AI invocation API. |

Permission rules:

- Default deny. A missing permission means the host function is unavailable.
- Scope strings are matched by typed values, not ad-hoc substring matching.
- PubSub scopes must resolve to Waddle node patterns listed in this spec.
- Network scopes are never client-controlled. OpenGraph fetches must enforce
  scheme, host allowlist, content type, redirect count, body byte limit, and
  timeout before the plugin receives sanitized metadata.
- AI scopes expose only server-owned profile IDs. Clients and plugin config
  never include API keys, OAuth tokens, model provider credentials, or endpoint
  secrets.
- Every denied permission emits an audit item and returns a typed extension
  error to the caller or runtime.

### Sample Plugin Nodes

| Plugin | Node | Access | Subscriber whitelist | Max | Payload |
| --- | --- | --- | --- | --- | --- |
| Link Board | `urn:waddle:link-board:1:waddle:{waddle-id}:links` | `whitelist` | Waddle members | `max` | `urn:waddle:link-board:1` |
| Link Board | `urn:waddle:link-board:1:waddle:{waddle-id}:boards` | `whitelist` | Waddle members | `256` | `urn:waddle:link-board:1` |
| Link Board | `urn:waddle:link-board:1:waddle:{waddle-id}:tasks:{board-id}` | `whitelist` | Waddle members | `max` | `urn:waddle:link-board:1` |
| Link Board | `urn:waddle:link-board:1:waddle:{waddle-id}:opengraph-cache` | `whitelist` | Waddle members | `max` | `urn:waddle:link-board:1` |
| Standard AI Chatbot | `urn:waddle:ai-chatbot:1:waddle:{waddle-id}:profiles` | `whitelist` | Waddle admins | `32` | `urn:waddle:ai-chatbot:1` |
| Standard AI Chatbot | `urn:waddle:ai-chatbot:1:waddle:{waddle-id}:runs` | `whitelist` | Waddle members | `1000` | `urn:waddle:ai-chatbot:1` |
| Decision Polls | `urn:waddle:decision-polls:1:waddle:{waddle-id}:polls` | `whitelist` | Waddle members | `max` | `urn:waddle:decision-polls:1` |
| Decision Polls | `urn:waddle:decision-polls:1:waddle:{waddle-id}:results` | `whitelist` | Waddle members | `max` | `urn:waddle:decision-polls:1` |
| Decision Polls | `urn:waddle:decision-polls:1:waddle:{waddle-id}:votes:{poll-id}` | `whitelist` | Waddle admins | `max` | `urn:waddle:decision-polls:1` |

Every sample plugin node uses `pubsub#publish_model=publishers`,
`pubsub#persist_items=1`, `pubsub#deliver_payloads=1`,
`pubsub#notify_retract=1`, `pubsub#notify_delete=1`, and
`pubsub#send_last_published_item=on_sub`.

Users do not publish directly to these nodes. User actions go through XEP-0050
`urn:waddle:extension:1:invoke`; the server validates membership and then
publishes typed items as the authorized publisher.

## Lifecycle

Implement lifecycle as explicit typed states, not booleans scattered across
handlers.

| State | How Entered | Runtime Behavior |
| --- | --- | --- |
| `cataloged` | Catalog item published with verified OCI digest metadata. | Visible to admins, not installed for any Waddle. |
| `installing` | Admin completes `admin:install`; server begins OCI verification. | No triggers, routes, or bot JIDs are active. |
| `installed` | OCI and manifest verification succeeds and installation item is published. | Config can be edited; runtime remains inactive until enabled. |
| `enabled` | Admin completes `admin:set-enabled` with `true`. | Granted triggers, routes, bot JID, and PubSub writers are active. |
| `disabled` | Admin disables or server trips a policy failure. | Existing PubSub state and archived messages remain; new triggers are blocked. |
| `updating` | Admin installs a new digest for the same plugin id. | Old digest continues serving active views until the new digest reaches `installed`. |
| `uninstalled` | Admin confirms uninstall. | Triggers and routes are removed; plugin data nodes remain unless a later purge command is added. |

Lifecycle rules:

- Every transition publishes an `<audit/>` item with actor bare JID, previous
  state, new state, plugin id, digest, and reason.
- `enabled` requires an installed grant, verified artifact cache, configured
  PubSub nodes, and a bot JID reservation when `bot.respond` is granted.
- Runtime traps, permission denials, OCI digest failures, or repeated timeouts
  move the plugin to `disabled` only when policy says the failure is fatal.
  Non-fatal enrichment failures are fail-open for the message.
- Updates are digest swaps. Do not mutate an installed artifact in place.
- Uninstall never rewrites MAM archives and never deletes PubSub state by
  default.

## Route Model

There are two route families: immutable artifact routes over HTTP(S), and
mutable action/state routes over XMPP.

HTTP(S) artifact routes:

| Route | Methods | Purpose |
| --- | --- | --- |
| `/_extensions/{plugin-id}/{digest}/manifest.json` | `GET`, `HEAD` | Verified declarative manifest for clients that need static metadata. |
| `/_extensions/{plugin-id}/{digest}/assets/{path}` | `GET`, `HEAD` | Verified icons, thumbnails, and static media from the OCI artifact. |
| `/_extensions/{plugin-id}/{digest}/artifacts/{artifact-id}` | `GET`, `HEAD` | Immutable generated artifacts. |

HTTP route rules:

- `{digest}` is the installed OCI digest without the `sha256:` prefix. It must
  map to an enabled or historically referenced installed grant.
- No `POST`, `PUT`, `PATCH`, `DELETE`, query-string command dispatch, cookies,
  user secrets, or mutable JSON APIs are allowed on extension HTTP routes.
- Responses must be immutable and cacheable. Use content types from verified
  artifact metadata and reject HTML, JavaScript, CSS, and iframe documents.
- Authorization is still required for member-only artifacts. Public catalog
  icons may be served without Waddle membership only if the catalog item says
  they are public.

XMPP route table:

| User Intent | XMPP Route | Backing Handler |
| --- | --- | --- |
| Discover extension component | `disco#items` to account domain | Domain disco handler. |
| Discover protocol support | `disco#info` to `extensions.<domain>` | Extension disco handler. |
| Discover commands | `disco#items` node `http://jabber.org/protocol/commands` | Command discovery handler. |
| Install/configure/enable/uninstall | XEP-0050 admin command nodes | Extension command handler. |
| Invoke a launch/action | XEP-0050 `urn:waddle:extension:1:invoke` | Launch lookup and runtime dispatcher. |
| Read extension state | XEP-0060 items on extension nodes | PubSub handler with Waddle membership authz. |
| Observe state changes | XEP-0060 event notifications | PubSub notification fanout. |
| Send bot response | Normal XMPP `message` from plugin bot JID | Server message dispatcher. |

Declarative client routes:

- A plugin may declare route ids such as `board`, `chat`, and `poll-results`
  inside its manifest.
- Route ids are inert descriptors. Opening a client route reads PubSub state
  and renders Waddle-native components. It does not load plugin HTML or JS.
- Route actions must map to XEP-0050 `invoke` with a plugin id, action id, and
  typed form fields.
- Route visibility is computed from the installed grant plus requester
  membership. Hidden routes must not be discoverable through client-side
  artifact inspection alone.

## Declarative UI Surface Model

Clients render extension UI from a small schema owned by Waddle. The schema is
data, not executable code.

Allowed surface elements:

- `text`, `markdown-safe`, `image`, `link-preview`, `button`, `button-group`,
  `form`, `field`, `select`, `progress`, `task-board`, `poll-results`, and
  `empty-state`.

Rules:

- Descriptors are parsed into typed Rust and TypeScript values before use.
- Text is escaped by the client renderer. Markdown-safe allows only the
  existing Waddle safe subset; no raw HTML.
- Buttons and forms may only invoke declared XEP-0050 actions.
- Images and media must reference immutable artifact routes with digests.
- No element may embed iframe URLs, script URLs, inline scripts, style blocks,
  remote fonts, event handler strings, arbitrary CSS classes, or arbitrary DOM.

## XEP-0050 Command Flows

All commands are sent to `extensions.<domain>`. All data forms use:

```xml
<field var='FORM_TYPE' type='hidden'>
  <value>urn:waddle:extension:1</value>
</field>
```

Use `waddle#op` to distinguish framework form intent. This keeps one framework
namespace while avoiding additional Waddle form namespaces.

### Common Command Rules

- Command IQs require a bound full JID. Pre-bind requests return
  `not-authorized`.
- Every `execute` starts a XEP-0050 session and returns `status='executing'`
  when input is required.
- Subsequent `next`, `prev`, `complete`, and `cancel` requests must include the
  `sessionid`.
- `cancel` returns `status='canceled'` and deletes session state.
- Unknown command node returns `item-not-found`.
- Unauthorized command node returns `forbidden`, not an empty success.
- Expired or unknown session returns XEP-0050 `<session-expired/>` or
  `<bad-sessionid/>` in the commands namespace.
- Command forms must never include `text-private` fields or fields whose var
  contains `secret`, `token`, `password`, `api_key`, `apikey`, or `credential`.

### `admin:list`

Node: `urn:waddle:extension:1:admin:list`

Execute without form payload:

```xml
<iq type='set' to='extensions.example.com' id='list-1'>
  <command xmlns='http://jabber.org/protocol/commands'
           node='urn:waddle:extension:1:admin:list'
           action='execute'/>
</iq>
```

Return `status='completed'` with a result form:

```xml
<command xmlns='http://jabber.org/protocol/commands'
         node='urn:waddle:extension:1:admin:list'
         status='completed'>
  <x xmlns='jabber:x:data' type='result'>
    <field var='FORM_TYPE' type='hidden'>
      <value>urn:waddle:extension:1</value>
    </field>
    <field var='waddle#op'><value>list</value></field>
    <reported>
      <field var='waddle#plugin_id' label='Plugin'/>
      <field var='waddle#name' label='Name'/>
      <field var='waddle#enabled' label='Enabled'/>
      <field var='waddle#version' label='Version'/>
      <field var='waddle#payload_ns' label='Payload Namespace'/>
    </reported>
    <item>
      <field var='waddle#plugin_id'><value>link-board</value></field>
      <field var='waddle#name'><value>Link Board</value></field>
      <field var='waddle#enabled'><value>true</value></field>
      <field var='waddle#version'><value>1.0.0</value></field>
      <field var='waddle#payload_ns'><value>urn:waddle:link-board:1</value></field>
    </item>
  </x>
</command>
```

### `admin:install`

Node: `urn:waddle:extension:1:admin:install`

Step 1 `execute` returns `status='executing'` and this form:

```xml
<x xmlns='jabber:x:data' type='form'>
  <field var='FORM_TYPE' type='hidden'><value>urn:waddle:extension:1</value></field>
  <field var='waddle#op' type='hidden'><value>install</value></field>
  <field var='waddle#waddle_id' type='text-single' label='Waddle ID'/>
  <field var='waddle#plugin_id' type='text-single' label='Plugin ID'/>
  <field var='waddle#artifact_oci' type='text-single' label='OCI Artifact Digest'/>
  <field var='waddle#wasm_digest' type='text-single' label='WASM Component Digest'/>
  <field var='waddle#manifest_digest' type='text-single' label='Manifest Digest'/>
  <field var='waddle#wit_world' type='text-single' label='WIT World'/>
  <field var='waddle#payload_ns' type='text-single' label='Payload Namespace'/>
  <field var='waddle#requested_capabilities' type='list-multi' label='Capabilities'>
    <option><value>message.enrich</value></option>
    <option><value>message.observe</value></option>
    <option><value>commands</value></option>
    <option><value>launch</value></option>
    <option><value>bot.respond</value></option>
    <option><value>pubsub.read</value></option>
    <option><value>pubsub.publish</value></option>
    <option><value>artifact.reference</value></option>
    <option><value>channels.read</value></option>
    <option><value>members.read</value></option>
    <option><value>presence.read</value></option>
    <option><value>message.send</value></option>
    <option><value>mam.query</value></option>
    <option><value>roster.read</value></option>
    <option><value>bot.presence</value></option>
    <option><value>ui.declarative</value></option>
  </field>
  <field var='waddle#requested_permissions' type='list-multi' label='Permissions'>
    <option><value>message.enrich</value></option>
    <option><value>message.observe</value></option>
    <option><value>mam.read.context</value></option>
    <option><value>bot.send.message</value></option>
    <option><value>pubsub.read</value></option>
    <option><value>pubsub.publish</value></option>
    <option><value>net.fetch.opengraph</value></option>
    <option><value>artifact.write</value></option>
    <option><value>extension.provider.config</value></option>
  </field>
</x>
```

Step 2 `complete` with `type='submit'` installs only if:

- requester is Waddle admin,
- `artifact_oci` is an OCI digest reference and has matching `wasm_digest` and
  `manifest_digest`,
- `wit_world` is `waddle:extension@1.0.0#world:waddle-extension`,
- `payload_ns` starts with `urn:waddle:` and is one of the sample payload
  namespaces for this slice,
- requested capabilities are allowed for that sample plugin,
- requested permissions are declared by the component and approved by policy,
- no submitted field is a secret field.

Return `status='completed'` with `waddle#installed=true`, then publish an
`<extension/>` item to the `installations` node and an `<audit/>` item to the
`audit` node.

### `admin:configure`

Node: `urn:waddle:extension:1:admin:configure`

Step 1 asks for `waddle#waddle_id` and `waddle#plugin_id`. Step 2 returns a
plugin-specific non-secret config form. Step 3 stores config in:

`urn:waddle:extension:1:waddle:{waddle-id}:plugin:{plugin-id}:config`

Use these common fields:

- `waddle#enabled_rooms`: `jid-multi`
- `waddle#bot_nick`: `text-single`
- `waddle#rate_limit_per_minute`: `text-single`
- `waddle#ai_profile`: `list-single`, for AI plugins only. Values are
  server-side profile IDs, not credentials.

### `admin:set-enabled`

Node: `urn:waddle:extension:1:admin:set-enabled`

Single form with `waddle#waddle_id`, `waddle#plugin_id`, and
`waddle#enabled` boolean. Completion updates the `installations` item and
publishes an audit item. Disabling a plugin prevents new triggers and launches
but does not rewrite archived messages.

### `admin:uninstall`

Node: `urn:waddle:extension:1:admin:uninstall`

Two-step form:

1. collect `waddle#waddle_id` and `waddle#plugin_id`,
2. require `waddle#confirm` with value `true`.

Completion removes the installation item, disables trigger routing, and
publishes an audit item. Plugin PubSub nodes are not deleted unless a separate
admin purge command is added in a later spec.

### `invoke`

Node: `urn:waddle:extension:1:invoke`

This is the only client launch/action path. Clients do not POST to HTTP and do
not publish action items directly to PubSub.

Invocation from a message launch:

```xml
<iq type='set' to='extensions.example.com' id='invoke-1'>
  <command xmlns='http://jabber.org/protocol/commands'
           node='urn:waddle:extension:1:invoke'
           action='execute'>
    <x xmlns='jabber:x:data' type='submit'>
      <field var='FORM_TYPE' type='hidden'><value>urn:waddle:extension:1</value></field>
      <field var='waddle#op'><value>invoke</value></field>
      <field var='waddle#waddle_id'><value>waddle-123</value></field>
      <field var='waddle#room_jid'><value>pub@muc.example.com</value></field>
      <field var='waddle#message_stanza_id'><value>archive-id-456</value></field>
      <field var='waddle#launch_id'><value>vote-a</value></field>
      <field var='payload#choice_id'><value>a</value></field>
    </x>
  </command>
</iq>
```

The server must load the archived message by `message_stanza_id`, find the
launch with `launch_id`, verify requester membership, verify launch expiry, and
then execute the plugin action. The client must not echo or invent payload XML.

Direct invocation not tied to a launch is allowed only when the plugin manifest
declares `commands`. In that case `execute` with `waddle#plugin_id` and
`waddle#action` returns the plugin action form before any state is changed.

## Message Enrichment Wire Shape

The server adds at most one direct framework payload to each message:

```xml
<extensions xmlns='urn:waddle:extension:1' version='1'>
  ...
</extensions>
```

Each plugin contribution is an `<enrichment/>` child. Plugin payload elements
live inside `<payload/>`, and launch descriptors live inside `<launch/>`.

Example Link Board enriched groupchat message:

```xml
<message from='pub@muc.example.com/alice'
         to='bob@example.com/device'
         type='groupchat'
         id='msg-1'>
  <body>Read https://example.org/post</body>
  <origin-id xmlns='urn:xmpp:sid:0' id='client-origin-1'/>
  <stanza-id xmlns='urn:xmpp:sid:0'
             by='pub@muc.example.com'
             id='archive-id-456'/>
  <extensions xmlns='urn:waddle:extension:1' version='1'>
    <enrichment id='enrich-1'
                plugin='link-board'
                capability='message.enrich'
                payload-ns='urn:waddle:link-board:1'
                created='2026-04-27T10:00:00Z'>
      <source stanza-id='archive-id-456'
              by='pub@muc.example.com'
              body-start='5'
              body-end='29'/>
      <payload>
        <link xmlns='urn:waddle:link-board:1'
              url='https://example.org/post'
              title='Example Post'
              site='Example'
              image='https://example.com/_extensions/link-board/def456/assets/thumb.png'
              image-sha256='def456'/>
      </payload>
      <launch id='save-link'
              plugin='link-board'
              action='save-link'
              command-node='urn:waddle:extension:1:invoke'
              label='Save link'
              expires-at='2026-04-28T10:00:00Z'>
        <context waddle-id='waddle-123'
                 room='pub@muc.example.com'
                 stanza-id='archive-id-456'/>
        <payload>
          <save-link xmlns='urn:waddle:link-board:1'
                     url='https://example.org/post'/>
        </payload>
      </launch>
    </enrichment>
  </extensions>
</message>
```

Implementation requirements:

- If multiple plugins enrich a message, append multiple `<enrichment/>`
  children under the same `<extensions/>` element.
- Do not add direct `<link/>`, `<poll/>`, or other plugin payload children to
  `<message/>`.
- Reject user-authored messages that contain a direct
  `<extensions xmlns='urn:waddle:extension:1'>` payload with `bad-request`.
- Do not re-enrich a message that already has a framework `<extensions/>`
  payload from the server.
- Enrichment is fail-open. If a plugin times out or traps, deliver the original
  message without that plugin's enrichment.
- Enrichment must finish before MAM archive write and before carbons are built.

## Launch Payloads

Launch descriptors are inert metadata until invoked through XEP-0050. A launch
descriptor must include `id`, `plugin`, `action`, `command-node`, and `context`.
It may include a plugin payload child under `<payload/>`.

### Link Board

Message enrichment payload:

```xml
<link xmlns='urn:waddle:link-board:1'
      url='https://example.org/post'
      title='Example Post'
      site='Example'
      description='Short OpenGraph description'
      image='https://example.com/_extensions/link-board/def456/assets/thumb.png'
      image-sha256='def456'
      og-cache-item='og-https-example-org-post'/>
```

Launches:

```xml
<launch id='save-link' plugin='link-board' action='save-link'
        command-node='urn:waddle:extension:1:invoke' label='Save link'>
  <context waddle-id='waddle-123' room='pub@muc.example.com' stanza-id='archive-id-456'/>
  <payload>
    <save-link xmlns='urn:waddle:link-board:1' url='https://example.org/post'/>
  </payload>
</launch>
<launch id='create-task' plugin='link-board' action='create-task'
        command-node='urn:waddle:extension:1:invoke' label='Create task'>
  <context waddle-id='waddle-123' room='pub@muc.example.com' stanza-id='archive-id-456'/>
  <payload>
    <task-candidate xmlns='urn:waddle:link-board:1'
                    url='https://example.org/post'
                    title='Review Example Post'/>
  </payload>
</launch>
```

Invoke fields:

- `save-link`: `payload#board_id` optional, `payload#note` optional.
- `create-task`: `payload#board_id` required, `payload#status` optional with
  default `todo`, `payload#assignee` optional bare JID.

Completion publishes a `<link/>` item to the `links` node, publishes sanitized
OpenGraph metadata to `opengraph-cache`, and for `create-task` publishes a
`<task/>` item to `tasks:{board-id}`. The plugin may fetch OpenGraph metadata
only through the server host function guarded by `net.fetch.opengraph`; clients
never fetch or inject metadata on behalf of the plugin.

Declarative board surface:

```xml
<view xmlns='urn:waddle:link-board:1'
      route='board'
      board-id='board-1'
      title='Launch Links'>
  <column id='todo' label='Todo'/>
  <column id='doing' label='Doing'/>
  <column id='done' label='Done'/>
</view>
```

The client renders this as Waddle-native task-board UI. It must not load iframe
content from the link URL.

### Standard AI Chatbot

Bot answer payload:

```xml
<assistant-answer xmlns='urn:waddle:ai-chatbot:1'
                  run-id='run-1'
                  profile='default'
                  context-source='mam'/>
```

Launch:

```xml
<launch id='ask-followup' plugin='ai-chatbot' action='ask-followup'
        command-node='urn:waddle:extension:1:invoke' label='Ask follow-up'>
  <context waddle-id='waddle-123' room='pub@muc.example.com' stanza-id='archive-id-ai-1'/>
</launch>
```

Invoke fields: `payload#question` is required for follow-up form submissions.
Provider integration is owned by the extension. Users never submit model API
keys or provider tokens through Waddle clients.

The standard chatbot is intentionally plain: one bot persona, mention/reply/DM
triggers, optional bounded MAM context, text answer messages, and no custom
artifact state. This keeps it useful as the baseline AI example.

### Decision Polls

Poll message payload:

```xml
<poll xmlns='urn:waddle:decision-polls:1'
      poll-id='poll-1'
      mode='single'
      closes-at='2026-04-27T11:00:00Z'>
  <question>Ship the extension framework this week?</question>
  <option id='yes'>Yes</option>
  <option id='no'>No</option>
</poll>
```

Launch per option:

```xml
<launch id='vote-yes' plugin='decision-polls' action='vote'
        command-node='urn:waddle:extension:1:invoke' label='Vote yes'>
  <context waddle-id='waddle-123' room='pub@muc.example.com' stanza-id='archive-id-poll-1'/>
  <payload>
    <vote-request xmlns='urn:waddle:decision-polls:1'
                  poll-id='poll-1'
                  option-id='yes'/>
  </payload>
</launch>
```

Invoke fields: no extra fields for single-choice button votes. Completion
publishes a vote item to the admin-only votes node and publishes aggregate
results to the results node. Member-visible result payloads must not expose
individual voter JIDs unless the poll was explicitly created as public-voter.

## MAM Behavior

MAM remains on user bare JIDs and MUC room JIDs, not on `extensions.<domain>`.

Requirements:

- For chat and groupchat messages, run extension enrichment before the final
  stanza is archived.
- Archive the final enriched stanza exactly once.
- MAM replay must not call extension enrichment again.
- MAM `<result xmlns='urn:xmpp:mam:2'/>` wraps the enriched message using
  XEP-0297 `<forwarded xmlns='urn:xmpp:forward:0'>`.
- The forwarded message must include the same framework `<extensions/>`
  payload, XEP-0359 `stanza-id`, replies, references, and mentions that were
  archived.
- XEP-0050 IQ commands are not archived as chat messages.
- Bot-generated chat/groupchat responses are normal messages and are archived
  after stanza-id assignment.
- Disabling or uninstalling a plugin never rewrites old MAM results. Historic
  messages keep their existing framework payloads.

Example MAM replay:

```xml
<message from='pub@muc.example.com' to='bob@example.com/device'>
  <result xmlns='urn:xmpp:mam:2' queryid='q1' id='archive-id-456'>
    <forwarded xmlns='urn:xmpp:forward:0'>
      <delay xmlns='urn:xmpp:delay' stamp='2026-04-27T10:00:00Z'/>
      <message from='pub@muc.example.com/alice'
               type='groupchat'
               id='msg-1'>
        <body>Read https://example.org/post</body>
        <stanza-id xmlns='urn:xmpp:sid:0'
                   by='pub@muc.example.com'
                   id='archive-id-456'/>
        <extensions xmlns='urn:waddle:extension:1' version='1'>
          <enrichment id='enrich-1'
                      plugin='link-board'
                      capability='message.enrich'
                      payload-ns='urn:waddle:link-board:1'>
            <payload>
              <link xmlns='urn:waddle:link-board:1'
                    url='https://example.org/post'
                    title='Example Post'/>
            </payload>
          </enrichment>
        </extensions>
      </message>
    </forwarded>
  </result>
</message>
```

## Carbons Behavior

Follow XEP-0280 exactly for DM carbons:

- Carbons are opt-in per resource through
  `<enable xmlns='urn:xmpp:carbons:2'/>`.
- Build `sent` and `received` carbons after enrichment.
- The carbon wrapper must be:
  `<sent xmlns='urn:xmpp:carbons:2'><forwarded xmlns='urn:xmpp:forward:0'>...`
  or
  `<received xmlns='urn:xmpp:carbons:2'><forwarded xmlns='urn:xmpp:forward:0'>...`
- The forwarded original message contains the framework `<extensions/>`
  payload.
- Do not enrich the carbon wrapper itself.
- Do not carbon-copy groupchat messages.
- Respect `<private xmlns='urn:xmpp:carbons:2'/>` and
  `<no-copy xmlns='urn:xmpp:hints'/>`.
- If a message is excluded from carbons, it may still be enriched and archived
  normally unless it also carries storage hints that prevent storage.

## Bot Trigger Rules

Bot identities are XMPP JIDs on the extension component:

- `link-board@extensions.<domain>`
- `ai-chatbot@extensions.<domain>`
- `decision-polls@extensions.<domain>`

Messages from bots use normal XMPP chat/groupchat messages with `from` set to
the bot JID or the bot's MUC occupant JID. They must pass through the same MAM,
stanza-id, reply, references, and carbons rules as other server-generated
messages.

Allowed triggers:

| Trigger | Wire source | Rule |
| --- | --- | --- |
| Slash command | Normal message body starting with `/` | First token must match a registered plugin command alias. Server intercepts before normal room broadcast unless plugin asks to echo. |
| Mention | XEP-0513 `urn:xmpp:mentions:0` mention targeting the bot JID, or configured body fallback | Triggers `bot.respond` for that bot only. |
| Reply | XEP-0461 reply to a bot-authored message in the same room/thread | Triggers that bot if it has `bot.respond`. |
| DM to bot | `type='chat'` to bot bare/full JID | Triggers that bot if requester can access the Waddle context or the bot supports personal DM mode. |
| Message enrichment | Accepted message body with supported content such as links | Triggers only `message.enrich`; it must not send a bot reply by itself. |
| Launch | XEP-0050 `invoke` command | Runs only the launch/action identified by archived `message_stanza_id` plus `launch_id`. |
| PubSub event | Published item on a node the plugin is granted to read | Internal server trigger; clients still observe resulting state via PubSub notifications. |

Disallowed triggers:

- Standard AI Chatbot must not respond to every message passively.
- Link Board must not post a chat reply merely because a link appeared.
- Plugins must not receive messages from rooms where they are not installed.
- Plugins must not use HTTP webhooks for Waddle control or user actions.
- Plugins must not rely on client-supplied secrets.

Sample plugin trigger matrix:

| Plugin | Capabilities | Triggers |
| --- | --- | --- |
| Link Board | `message.enrich`, `launch`, `commands`, `pubsub.publish`, `artifact.reference`, `ui.declarative` | OpenGraph link enrichment; `/links`; `/board`; `save-link` and `create-task` launches. |
| Standard AI Chatbot | `commands`, `launch`, `bot.respond`, `message.observe`, `channels.read`, `members.read`, `presence.read`, `message.send`, `mam.query`, `roster.read`, `bot.presence` | Mention, DM, reply, `/ai`; follow-up launch. |
| Decision Polls | `commands`, `launch`, `bot.respond`, `pubsub.publish`, `ui.declarative` | `/poll`; vote launches; close poll command/timer. |

## Implementation Work Items

Do not implement in this planning PR. When implementation starts, split into
small PRs in this order:

1. Add `extensions.<domain>` to service domain config and server disco items.
2. Add extension service disco info/items and XEP-0050 command-node disco.
3. Add extension PubSub node provisioning, metadata, config, and access checks.
4. Add XEP-0050 handlers for `admin:list`, `admin:install`,
   `admin:configure`, `admin:set-enabled`, `admin:uninstall`, and `invoke`.
5. Add OCI-by-digest artifact verification, content-addressed cache, and
   immutable `/_extensions/{plugin-id}/{digest}/...` artifact routes.
6. Add lifecycle state machine, audit items, permission grants, and runtime
   host-function checks.
7. Add declarative UI schema parsing/render fixtures and reject iframe/HTML/JS
   surfaces.
8. Replace raw extension embeds with the single
   `urn:waddle:extension:1` message envelope.
9. Add launch lookup by archived stanza ID plus launch ID.
10. Add bot trigger dispatch and sample plugin trigger declarations.
11. Add sample plugin PubSub payload parsers/builders and fixtures.
12. Wire MAM and carbons tests to prove enrichment is archived/copied exactly.

Expected code areas:

- `server/crates/waddle-xmpp-core/src/disco/*`
- `server/crates/waddle-xmpp-core/src/pubsub/*`
- `server/crates/waddle-xmpp/src/xep/xep0050.rs`
- `server/crates/waddle-xmpp/src/commands/*`
- `server/crates/waddle-server/src/server/routes/websocket/handlers/iq.rs`
- `server/crates/waddle-server/src/server/routes/websocket/handlers/message.rs`
- `server/crates/waddle-server/src/server/routes/extensions.rs`
- `server/crates/waddle-server/src/extension_lifecycle.rs`
- `server/crates/waddle-server/src/extension_permissions.rs`
- `server/crates/waddle-extensions/src/*`
- `server/wit/waddle-extension.wit`

## Required Tests

Every implementation PR that touches this slice must add or update Rust tests.
Use typed XML builders/parsers in implementation and assertions; do not build
production XML with string concatenation.

### Disco Tests

Add `server/crates/waddle-server/tests/waddle_extension_disco_ws.rs`.

Must assert:

- `disco#items` on `example.com` includes `extensions.example.com`.
- `disco#info` on `extensions.example.com` returns exactly the identities and
  features listed in this spec.
- `disco#items` on `extensions.example.com` node
  `http://jabber.org/protocol/commands` returns the six command nodes.
- `disco#info` for every command node includes
  `automation/command-node`, `http://jabber.org/protocol/commands`,
  `jabber:x:data`, and `urn:waddle:extension:1`.
- No framework response advertises non-approved `urn:waddle:*` namespaces.
  During migration, legacy GitHub enrichment may still be present only behind
  the server-side compatibility bridge; it must not be used as a new framework
  namespace.

### PubSub Tests

Add `server/crates/waddle-server/tests/waddle_extension_pubsub_ws.rs`.

Must assert:

- Framework and sample plugin nodes are created with the exact node names in
  this spec.
- Node config forms use `FORM_TYPE=http://jabber.org/protocol/pubsub#node_config`.
- Node metadata forms use `FORM_TYPE=http://jabber.org/protocol/pubsub#meta-data`.
- Waddle member nodes use `pubsub#access_model=whitelist`.
- Catalog node uses `pubsub#access_model=open`.
- All nodes use `pubsub#publish_model=publishers`.
- Non-admin members cannot retrieve admin-only config, votes, submissions, or
  profiles nodes.
- Users cannot publish directly to sample plugin nodes.
- Server/plugin publisher can publish typed payloads to granted nodes.

### XEP-0050 Tests

Add `server/crates/waddle-server/tests/waddle_extension_commands_ws.rs`.

Must assert:

- Pre-bind command IQ returns `not-authorized`.
- Unknown command node returns `item-not-found`.
- Unauthorized admin command returns `forbidden`.
- `admin:list` completes with a `jabber:x:data` result form.
- `admin:install` returns an executing form, accepts valid `artifact_oci`,
  `wasm_digest`, `manifest_digest`, and `wit_world` fields, and rejects missing
  digest fields.
- `admin:install` rejects tag-only OCI references, mutable/non-Waddle payload
  namespaces, unapproved permissions, and secret fields.
- `admin:configure` rejects fields containing `secret`, `token`, `password`,
  `api_key`, `apikey`, or `credential`.
- `invoke` with invalid `message_stanza_id` or `launch_id` returns
  `item-not-found`.
- `invoke` with expired launch returns `not-allowed`.
- `cancel` returns `status='canceled'` and removes session state.

### Negotiation Tests

Add `server/crates/waddle-server/tests/waddle_extension_negotiation_ws.rs`.

Must assert:

- A client that discovers `extensions.example.com` can negotiate the framework
  by checking `urn:waddle:extension:1`, XEP-0050, XEP-0060, and XEP-0004
  features in order.
- Command discovery uses the fixed
  `http://jabber.org/protocol/commands` node and every returned command item
  has `jid='extensions.example.com'`.
- A client that does not understand a plugin payload namespace can still render
  the original message body and ignore that plugin surface.
- A client cannot invoke a launch whose action is absent from the installed
  manifest route/action table.
- Future or unknown framework namespace advertisements do not change version 1
  behavior.

### OCI/WASM Artifact Tests

Add `server/crates/waddle-server/tests/waddle_extension_artifacts.rs`.

Must assert:

- OCI references without `@sha256:` are rejected.
- OCI manifest digest, config digest, WIT digest, manifest digest, and WASM
  component digest are all verified before cache write.
- A digest mismatch aborts install and leaves no partial cache entry.
- An artifact with zero or multiple WASM component layers is rejected.
- Runtime instantiation receives no ambient filesystem, environment, network,
  clock, or secret access.
- Immutable `/_extensions/{plugin-id}/{digest}/...` routes serve only verified
  files for authorized requesters.
- Extension HTTP routes reject `POST`, `PUT`, `PATCH`, `DELETE`, HTML,
  JavaScript, CSS, iframe documents, and mutable JSON command endpoints.

### Permission and Lifecycle Tests

Add `server/crates/waddle-server/tests/waddle_extension_permissions_lifecycle.rs`.

Must assert:

- Installation stores the admin-approved intersection of requested and
  component-declared capabilities/permissions.
- Missing permissions make the corresponding runtime host function unavailable.
- PubSub read/publish permissions are checked against typed node patterns.
- OpenGraph fetch permission enforces scheme, host allowlist, redirect count,
  content type, byte limit, and timeout.
- AI permissions expose only server-owned profile IDs and never provider
  secrets.
- Lifecycle transitions publish audit items for cataloged, installing,
  installed, enabled, disabled, updating, and uninstalled states.
- Updating installs a new digest without mutating the old digest cache.
- Uninstall disables triggers/routes but leaves PubSub data and MAM archives
  unchanged.

### Declarative UI and Route Tests

Add `server/crates/waddle-server/tests/waddle_extension_ui_routes.rs` and
`chat` component tests when the client renderer exists.

Must assert:

- Declarative route ids map to PubSub reads and XEP-0050 actions only.
- The renderer accepts allowed elements and rejects iframe URLs, raw HTML,
  scripts, style blocks, arbitrary CSS, event handler strings, remote fonts,
  and plugin DOM directives.
- Images and artifact-backed media must reference immutable artifact routes with
  digests.
- Route visibility follows Waddle membership and installed grants.
- Hidden routes are not exposed through command discovery, PubSub state, or
  client manifest rendering for unauthorized users.

### Message Enrichment Tests

Add `server/crates/waddle-server/tests/waddle_extension_enrichment_ws.rs`.

Must assert:

- A message with a link gets exactly one top-level
  `<extensions xmlns='urn:waddle:extension:1'>`.
- Link Board payload appears only under
  `extensions/enrichment/payload/link`.
- No direct top-level `urn:waddle:link-board:1` payload is present.
- Link Board can create a task from a launch and persists sanitized
  OpenGraph metadata to the `opengraph-cache` node.
- Multiple enrichers produce multiple `<enrichment/>` children under one
  framework envelope.
- Client-authored `<extensions xmlns='urn:waddle:extension:1'>` is rejected.
- Failed or timed-out enrichment still delivers the original message.
- Re-sending an already server-enriched message through MAM or carbons does not
  add a second envelope.

### MAM Tests

Add `server/crates/waddle-server/tests/waddle_extension_mam_ws.rs`.

Must assert:

- MAM replay returns the enriched archived stanza inside
  `urn:xmpp:mam:2` result and `urn:xmpp:forward:0` forwarded.
- The replayed stanza keeps the original framework envelope and plugin payload.
- MAM replay does not call the extension manager.
- Disabling a plugin after archive write does not remove enrichment from old
  MAM results.
- XEP-0050 command IQs do not appear as MAM chat results.
- Bot messages appear in MAM as normal messages with stanza IDs.

### Carbons Tests

Add `server/crates/waddle-server/tests/waddle_extension_carbons_ws.rs`.

Must assert:

- For DMs, enrichment happens before `sent` and `received` carbons are built.
- Carbon forwarded message contains the same framework envelope as the primary
  delivered message.
- Carbon wrappers themselves do not contain direct framework payloads outside
  the forwarded original.
- Groupchat messages are not carbon-copied.
- `<private xmlns='urn:xmpp:carbons:2'/>` and
  `<no-copy xmlns='urn:xmpp:hints'/>` suppress carbons without suppressing
  normal enrichment.

### Bot Trigger Tests

Add `server/crates/waddle-server/tests/waddle_extension_bot_triggers_ws.rs`.

Must assert:

- Link Board enriches link messages but does not post a bot reply.
- Standard AI Chatbot responds to mention, DM, reply, and `/ai`, but not to unrelated
  room messages.
- Decision Polls `/poll` creates a poll message, vote launch records a vote in
  the admin-only votes node, and member-visible results omit voter JIDs unless
  the poll is public-voter.
- Plugins do not trigger in rooms where they are not installed.

### Namespace Guard Tests

Add unit tests in the extension XML module once it exists.

Must assert:

- Framework envelope namespace is exactly `urn:waddle:extension:1`.
- Plugin payload namespaces start with `urn:waddle:` and match installed plugin
  manifests.
- Official XEP namespaces in extension flows are only the exact XEP namespaces
  listed in this spec.
- New framework payloads do not use the legacy `urn:waddle:github:0`
  namespace. Any remaining GitHub compatibility bridge must wrap output into
  `<extensions xmlns='urn:waddle:extension:1'>` before it reaches clients.

## Validation Commands

For implementation PRs, run at minimum:

```bash
cuenv exec -- cargo test -p waddle-xmpp
cuenv exec -- cargo test -p waddle-xmpp-core
cuenv exec -- cargo test -p waddle-server --test waddle_extension_disco_ws
cuenv exec -- cargo test -p waddle-server --test waddle_extension_pubsub_ws
cuenv exec -- cargo test -p waddle-server --test waddle_extension_commands_ws
cuenv exec -- cargo test -p waddle-server --test waddle_extension_negotiation_ws
cuenv exec -- cargo test -p waddle-server --test waddle_extension_artifacts
cuenv exec -- cargo test -p waddle-server --test waddle_extension_permissions_lifecycle
cuenv exec -- cargo test -p waddle-server --test waddle_extension_ui_routes
cuenv exec -- cargo test -p waddle-server --test waddle_extension_enrichment_ws
cuenv exec -- cargo test -p waddle-server --test waddle_extension_mam_ws
cuenv exec -- cargo test -p waddle-server --test waddle_extension_carbons_ws
cuenv exec -- cargo test -p waddle-server --test waddle_extension_bot_triggers_ws
cd chat && bun test && bun run lint && bun run build
```

The planning PR that adds this document does not need to implement or run those
tests.
