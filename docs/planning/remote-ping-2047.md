# Remote ping routing while federation is unavailable

Issue #2047 identifies a false success when a client sends an XMPP ping to a
foreign domain: a local handler answers using the foreign target as the sender.
Until S2S routing exists, reject that request with a truthful local error.

Keep the change limited to ping requests and preserve locally addressed pings,
MUC self-ping, existing local full-JID forwarding, and the no-response rule for
IQ results and errors. Use exact configured service domains rather than suffix
matching. Do not imply an attempted remote lookup or impersonate the foreign
domain in the response. Cover the WebSocket dispatch path and relevant XEP-0199
and XEP-0410 behavior; no federation transport is enabled by this change.
