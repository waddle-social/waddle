# Bundled fonts

Installed by packaging, not by the app: the Flatpak copies them to
`/app/share/fonts`, and `flake.nix` puts them on fontconfig's path with
`FONTCONFIG_FILE` (`packages.linux`, `devShells.linux`). `style.css` names the
families with system fallbacks.

| File | Family | Licence |
| --- | --- | --- |
| `BricolageGrotesque.ttf` | Bricolage Grotesque (display) | SIL OFL 1.1, `OFL-BricolageGrotesque.txt` |
| `AtkinsonHyperlegibleNext*.ttf` | Atkinson Hyperlegible Next (body) | SIL OFL 1.1, `OFL-AtkinsonHyperlegible.txt` |
| `AtkinsonHyperlegibleMono*.ttf` | Atkinson Hyperlegible Mono (code) | SIL OFL 1.1, `OFL-AtkinsonHyperlegible.txt` |

Source: `google/fonts` at 821aa6f71ab4593d623720de526b0ac67487dde6 (`ofl/bricolagegrotesque`,
`ofl/atkinsonhyperlegiblenext`, `ofl/atkinsonhyperlegiblemono`), the same families as
`design/src/astro-fonts.ts`.
