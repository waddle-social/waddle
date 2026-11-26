# Bundled symbolic icons

`hicolor/symbolic/actions/` holds copies of the GNOME Adwaita symbolic icons
the app uses (the names in `src/` and `data/ui/`). They sit in `hicolor`, the
fallback theme, so an icon theme that has an icon still wins and one that lacks
it no longer shows a broken-image glyph.

Source: adwaita-icon-theme, © GNOME Project, LGPL-3.0-or-later or
CC-BY-SA-3.0. `chat-bubbles-text-symbolic` and `external-link-symbolic` are
absent from older Adwaita, so they are drawn here for Waddle (AGPL-3.0-or-later).
