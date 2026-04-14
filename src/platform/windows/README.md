# CoreFS Windows

Dieser Ordner enthaelt die Windows-spezifische CoreFS-Integration.

Aktuell liegen hier:

- `mod.rs` — Windows-Laufwerksadapter fuer CoreFS-Images
- `subst`-basierte Laufwerksprojektion auf einen Windows-Laufwerksbuchstaben
- Session-Metadaten fuer Mount/Unmount inklusive Rueckschreiben ins Image

Der Pfad ist absichtlich getrennt von `linux_fuse.rs`, damit Windows-spezifische
Logik, CLI-Annahmen und spaetere WinFSP-/Dokan-/Treiberarbeit an einer Stelle
zusammenbleiben.
