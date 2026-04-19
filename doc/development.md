# Entwicklungsleitfaden

## Build

```bash
cargo check                          # Syntax-/Type-Check
cargo build                          # debug
cargo build --release                # optimiert
cargo fmt                            # Formatierung
cargo clippy --all-targets --workspace
cargo doc --workspace --open

# Nur Kern (no_std, für AnyOS-Kompatibilitätscheck)
cargo build -p corefs-core --no-default-features

# Mit Crypto (std)
cargo build -p corefs-core --features std
```

## Workspace

Crates:

```
corefs                 (Root, std, Binary+Lib, Linux-FUSE)
├── corefs-core        (no_std+alloc, Kernbibliothek)
├── corefs-cli         (CLI-Wrapper)
├── corefs-tools       (Host-Tools: backup, keys, mount)
├── corefs-std         (std-Wrapper)
├── corefs-fuse-proto  (Protocol-Stubs)
└── corefs-fuse-adapter(IPC-Adapter)
```

## Dependencies (Auszug)

- `corefs-core`: `serde`, `bincode 2`, `hashbrown`, optional `lz4_flex` (`compression`), optional `chacha20poly1305` (`crypto`)
- Root `corefs`: `corefs-core{features=["std"]}`, `serde_json`, `lz4_flex[frame]`, `chacha20poly1305[getrandom]`
- Linux-spezifisch: `fuser 0.14`, `libc 0.2` (nur `[target.'cfg(target_os="linux")'.dependencies]`)

Plattformspezifische Crates ausschliesslich über `cfg`-Targets. Im Kern (`domain/`, `storage/`, `services/`) sind `fuser` und `libc` verboten.

## Entwicklungsregeln (aus CLAUDE.md)

- **Sprache**: Rust, Edition 2024.
- Struktur, Testbarkeit und Wartbarkeit haben Vorrang vor Feature-Vollständigkeit.
- Möglichst vollständige Testabdeckung für vorhandene Implementierung anstreben.
- **Keine Abstraktion ohne konkreten Bedarf**.
- **Keine plattformspezifischen Annahmen** in `domain/` oder `storage/`.
- Unit-Tests neben dem Modul in `*_tests.rs`; E2E-Tests in `tests/`.

## Commit-Workflow

```bash
cargo test          # muss vollständig grün sein
git add <Dateien>
git commit -m "..."
```

**Kein Commit bei fehlschlagenden Tests.**

## Projektdokumente aktualisieren

| Datei | Anlass |
|---|---|
| [PROJECT_PROGRESS.md](../PROJECT_PROGRESS.md) | Umsetzungsstand, neue Features / Phasen |
| [features_corefs.md](../features_corefs.md) | Anforderungen / Feature-Wünsche |
| [PERFORMANCE_LOG.md](../PERFORMANCE_LOG.md) | automatisch via `benchmark` mit `--log` |
| `doc/` | Verhalten, Architektur, CLI, Format |

## Neuer Service / neues Modul (corefs-core)

1. Datei unter `corefs-core/src/services/<name>.rs` anlegen — `no_std + alloc`, über `platform::Clock`/`Rng` abstrahieren.
2. In `corefs-core/src/services/mod.rs` registrieren.
3. Test-Modul neben dem Modul (`<name>_tests.rs`).
4. Bei Bedarf `std`-Wrapper unter `src/services/<name>.rs` (Root-Crate) anlegen.
5. `CoreFsService` (`src/app/mod.rs`) erweitern, falls Fassaden-sichtbar.
6. `cargo test` → grün → Commit.

## Neues CLI-Kommando

1. Subcommand in [src/cli.rs](../src/cli.rs) hinzufügen.
2. Argument-Parsing und Dispatch.
3. Dokumentation in [doc/cli.md](cli.md) ergänzen.
4. Beispiel in [doc/examples.md](examples.md) ergänzen.

## Plattform-Adapter

Alles plattformspezifische gehört in `src/platform/` bzw. `corefs-*-adapter`-Crates:

```rust
#[cfg(target_os = "linux")]
mod linux_fuse;
```

Im Kern niemals `#[cfg(target_os = ...)]` verwenden.

## Bekannte Architektur-Drift

Siehe [architecture.md](architecture.md) — der Top-Level `src/domain/` ist Re-Export + std-Erweiterung, nicht vollständig eigenständig. Bei neuen Arbeiten an Domain-Typen immer zuerst in `corefs-core::domain` ergänzen, dann ggf. lokal re-exportieren.

## Referenzdateien

Bei Erweiterungen sind [features_corefs.md](../features_corefs.md) und [corefs_brainstorming.txt](../corefs_brainstorming.txt) zu konsultieren, um Anforderungen nicht zu übersehen.
