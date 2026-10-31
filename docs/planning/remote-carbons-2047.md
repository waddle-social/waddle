# Foreign Carbons controls while federation is unavailable

Issue #2047 also affects XEP-0280 controls: an IQ addressed to an unserved domain
can otherwise enable or disable the originating local resource's Carbons and
receive a false success from the named foreign target.

Extend #2049's preflight only to the registered Carbons namespace, after binding
and existing local full-JID routing but before any Carbons connection, registry
or cluster-mirror update. Reject foreign IQ get/set requests with a typed
`cancel/not-allowed` error, preserving ID and payload, swapping the target into
`from`, addressing the bound origin, and identifying the local generator in
`error.by`. This is local routing policy, not evidence of DNS or remote contact.
The configured home and service-domain inventory remains exact; no suffix
matching or guessed component addresses are introduced.

[XEP-0280 v1.0.1](https://xmpp.org/extensions/xep-0280.html) defines per-client
enable/disable controls. [RFC 6120 section 8.3.1](https://www.rfc-editor.org/rfc/rfc6120.html#section-8.3.1)
defines the swapped-address error form and forbids further errors in response
to errors. Keep the existing no-response handling for IQ results/errors and
the authentication error before resource binding. Run through ordinary frame
dispatch so Stream Management inbound settlement is unchanged.

Frame tests assert both connection and registry flags, including enabled and
disabled origins, local controls, configured domains, full-JID precedence,
no error loops and Stream Management progress. The native WebSocket test uses
SCRAM and real message/carbon delivery to verify that a rejected foreign disable
does not turn off an enabled resource.

This change preserves existing local bare-account/component handling and local
malformed-control behavior; it does not establish their complete XEP-0280
conformance. Other foreign IQ namespaces, peer IQ response routing, malformed
JID handling, grant authority, S2S and production federation remain separate.
