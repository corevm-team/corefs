# CoreFS Windows

Dieser Ordner enthaelt die Windows-spezifische CoreFS-Integration.

Aktuell liegen hier:

- `mod.rs` - Windows-Laufwerksadapter fuer CoreFS-Images
- `winfsp_backend.rs` - nativer WinFSP-Host fuer CoreFS-Images als Windows-Laufwerk
- Read-only- und Read-write-Mounts ohne `subst` und ohne Staging-Verzeichnis
- Wrapper-Skripte liegen separat unter `scripts/windows/` und verwenden die normale `corefs`-CLI

Der Pfad ist absichtlich getrennt von `linux_fuse.rs`, damit Windows-spezifische
Logik, CLI-Annahmen und WinFSP-Treiberarbeit an einer Stelle zusammenbleiben.

Build:

```powershell
cargo build --features windows-winfsp
```

Voraussetzungen:

- WinFSP 2.x Runtime
- Rust MSVC-Toolchain fuer Windows-Builds
- Keine `subst`-Fallbacks und kein Staging-Verzeichnis; CoreFS laeuft ueber den WinFSP-Treiber
- Keine lokale LLVM/libclang-Installation noetig; `vendor/winfsp-sys` nutzt die mitgelieferten Bindings
