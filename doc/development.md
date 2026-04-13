# Entwicklungsleitfaden

## Build

```bash
cargo check                # schneller Syntax-/Type-Check
cargo build                # debug
cargo build --release      # optimiert
cargo fmt                  # Formatierung
cargo clippy               # Linting
cargo doc --open           # Rust-Doku erzeugen und öffnen
```

## Abhängigkeiten

Aus [Cargo.toml](../Cargo.toml):

```toml
[package]
name = "corefs"
edition = "2024"

[dependencies]
bincode           = "1"
chacha20poly1305  = "0.10"
lz4_flex          = "0.11"
serde             = "1"
serde_json        = "1"

[target.'cfg(target_os = "linux")'.dependencies]
fuser = "0.14"
libc  = "0.2"
```

Plattformspezifische Crates ausschließlich über `cfg`-Targets. Im Kern (`domain/`, `storage/`, `services/`) sind `fuser` und `libc` verboten.

## Entwicklungsregeln (aus CLAUDE.md)

- **Sprache**: Rust, Edition 2024.
- Struktur, Testbarkeit und Wartbarkeit haben Vorrang vor Feature-Vollständigkeit.
- Möglichst vollständige Testabdeckung.
- **Keine Abstraktion ohne konkreten Bedarf** — keine spekulativen Generalisierungen.
- **Keine plattformspezifischen Annahmen** in `domain/` oder `storage/`.

## Commit-Workflow

Aus [CLAUDE.md](../CLAUDE.md):

```bash
cargo test          # muss vollständig grün sein
git add <geänderte Dateien>
git commit -m "..."
```

**Kein Commit bei fehlschlagenden Tests.**

## Projektdokumente aktualisieren

Bei relevanten Änderungen:

| Datei | Was aktualisieren |
|---|---|
| [PROJECT_PROGRESS.md](../PROJECT_PROGRESS.md) | Umsetzungsstand, neue Features |
| [features_corefs.md](../features_corefs.md) | Anforderungen / neue Features |
| [PERFORMANCE_LOG.md](../PERFORMANCE_LOG.md) | automatisch via `benchmark-log` |
| `doc/` | Wenn sich Architektur, CLI oder Verhalten ändert |

## Neuer Service / neues Modul

1. Neue Datei unter `src/services/<name>.rs` anlegen.
2. In [src/services/mod.rs](../src/services/mod.rs) registrieren.
3. Service in [src/app/mod.rs](../src/app/mod.rs) einbinden, falls extern sichtbar.
4. Unit-Tests in der selben Datei (`#[cfg(test)] mod tests`).
5. `cargo test` → grün → Commit.

## Neues CLI-Kommando

1. Subcommand in [src/cli.rs](../src/cli.rs) hinzufügen.
2. Argument-Parsing und Dispatch.
3. Dokumentation in [doc/cli.md](cli.md) ergänzen.
4. Beispiel in [doc/examples.md](examples.md) ergänzen.

## Plattform-Adapter

Alles plattformspezifische gehört in `src/platform/`:

```rust
#[cfg(target_os = "linux")]
mod linux_fuse;
```

Im Kern niemals `#[cfg(target_os = ...)]` verwenden.

## Referenzdateien

Bei Erweiterungen sind [features_corefs.md](../features_corefs.md) und [corefs_brainstorming.txt](../corefs_brainstorming.txt) zu konsultieren, um Anforderungen nicht zu übersehen.
