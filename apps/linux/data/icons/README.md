# Bundled symbolic icons

`hicolor/symbolic/actions/` holds copies of the GNOME Adwaita symbolic icons
the app uses (the names in `src/` and `data/ui/`). They sit in `hicolor`, the
fallback theme, so an icon theme that has an icon still wins and one that lacks
it no longer shows a broken-image glyph.

Source: adwaita-icon-theme, © GNOME Project, LGPL-3.0-or-later or
CC-BY-SA-3.0. Not bundled (absent from older Adwaita): `chat-bubbles-text-symbolic`,
`external-link-symbolic`.
