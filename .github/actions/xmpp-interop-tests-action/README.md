# Local XMPP Interop Action Wrapper

This local composite action is based on:
- https://github.com/XMPP-Interop-Testing/xmpp-interop-tests-action

Why we keep a local wrapper:
- Upstream hardcodes `-Dsinttest.securityMode=disabled`.
- `waddle-server` requires STARTTLS in compliance runs, so the upstream default fails immediately.

Local deltas:
- Added `securityMode` input (default: `required`).
- Added `trustedCertPath` input to import a self-signed cert into Java cacerts.

When upstream supports configurable security mode and cert trust directly, we can switch back to using upstream action verbatim.
