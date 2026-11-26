//! The Polar Huddle families (SIL OFL, `data/fonts/`): Bricolage Grotesque
//! for display text, Atkinson Hyperlegible Next for the body and Atkinson
//! Hyperlegible Mono for code.
//!
//! GTK has no `@font-face`, and the toolkit's font map is fontconfig's. The
//! files are embedded and copied to `$XDG_DATA_HOME/fonts/waddle/` before
//! GTK starts, where fontconfig looks for per-user fonts. That is the same
//! under `cargo run`, `nix run` and the Flatpak, with no wrapper
//! environment. `style.css` names the families with system fallbacks, so a
//! failed copy only costs the typography.

use std::io::Write;
use std::path::Path;

const FONTS: [(&str, &[u8]); 5] = [
    (
        "BricolageGrotesque.ttf",
        include_bytes!("../data/fonts/BricolageGrotesque.ttf"),
    ),
    (
        "AtkinsonHyperlegibleNext.ttf",
        include_bytes!("../data/fonts/AtkinsonHyperlegibleNext.ttf"),
    ),
    (
        "AtkinsonHyperlegibleNext-Italic.ttf",
        include_bytes!("../data/fonts/AtkinsonHyperlegibleNext-Italic.ttf"),
    ),
    (
        "AtkinsonHyperlegibleMono.ttf",
        include_bytes!("../data/fonts/AtkinsonHyperlegibleMono.ttf"),
    ),
    (
        "AtkinsonHyperlegibleMono-Italic.ttf",
        include_bytes!("../data/fonts/AtkinsonHyperlegibleMono-Italic.ttf"),
    ),
];

/// Copy the embedded fonts into the user's font directory. Call before GTK
/// initialises, so fontconfig sees them on its first scan.
pub(crate) fn install() {
    install_into(&gtk::glib::user_data_dir().join("fonts").join("waddle"));
}

fn install_into(dir: &Path) {
    if let Err(error) = std::fs::create_dir_all(dir) {
        tracing::warn!(%error, "cannot create the font directory");
        return;
    }
    for (name, bytes) in FONTS {
        let path = dir.join(name);
        if std::fs::read(&path).is_ok_and(|existing| existing == bytes) {
            continue;
        }
        if let Err(error) = write_atomic(&path, bytes) {
            tracing::warn!(%error, font = name, "cannot install a font");
        }
    }
}

/// Write beside the target and rename, so a reader never sees half a font.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("ttf.part");
    let mut file = std::fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fonts_are_written_once_and_repaired_when_changed() {
        let dir = std::env::temp_dir().join(format!("waddle-fonts-{}", std::process::id()));
        install_into(&dir);
        for (name, bytes) in FONTS {
            assert_eq!(std::fs::read(dir.join(name)).expect("written"), bytes);
        }
        let first = dir.join(FONTS[0].0);
        std::fs::write(&first, b"damaged").expect("damage");
        install_into(&dir);
        assert_eq!(std::fs::read(&first).expect("repaired"), FONTS[0].1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
