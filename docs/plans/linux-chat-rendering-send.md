# Linux chat rendering and sending fixes

- Render linked HTTPS images and animated GIFs with bounded downloads and safe row reuse.
- Join selected rooms and enable sending after self-presence confirms membership; surface failures and rejoin after reconnect.
- Improve native sidebar, message spacing, and composer presentation.

Implemented and verified with Linux/shared-core Clippy, 24 Linux tests, 39 shared timeline tests, rustfmt, Blueprint compilation, and offline GTK previews at desktop/narrow widths. Rejections supersede transport acknowledgements while actual reflections remain authoritative. Live authenticated message delivery and visual interaction remain unverified.
