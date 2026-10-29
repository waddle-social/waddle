# Remote ping routing while federation is unavailable

Issue #2047 identifies a false success when a client sends an XMPP ping to a
foreign domain: a local handler answers using the foreign target as the sender.
Until S2S routing exists, reject that request with a truthful local error.

Keep the change limited to ping requests and preserve locally addressed pings,
MUC self-ping, existing local full-JID forwarding, and the no-response rule for
IQ results and errors. Use exact configured service domains rather than suffix
matching. Do not imply an attempted remote lookup. Return an IQ error with the
original target as `from`, the bound sender as `to`, the original ID and payload,
and `error.by` identifying the home server.
[RFC 6120 section 8.3.1](https://www.rfc-editor.org/rfc/rfc6120.html#section-8.3.1)
specifies this swapped-address error form; it reports a local routing failure
rather than a successful response from the remote target. Use
`cancel/service-unavailable`, without claiming a DNS lookup or remote timeout.
Cover the WebSocket dispatch path and relevant
[XEP-0199 v2.0.1](https://xmpp.org/extensions/xep-0199.html) and XEP-0410 behavior;
no federation transport is enabled by this change.
