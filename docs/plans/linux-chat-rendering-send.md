# Linux chat rendering and sending fixes

- Render linked HTTPS images and animated GIFs with bounded downloads and safe row reuse.
- Join selected rooms and enable sending after self-presence confirms membership; surface failures and rejoin after reconnect.
- Improve native sidebar, message spacing, and composer presentation.

Implemented and verified with Linux Clippy, 24 tests, rustfmt, and Blueprint compilation. Live authenticated message delivery and visual interaction remain unverified.
