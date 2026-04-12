# CoreFS — Arbeitsanweisungen für Claude Code

## Projektübersicht

CoreFS ist ein natives Dateisystem, entwickelt in **Rust**, primär als Standard-Dateisystem für ein eigenes Betriebssystem (AnyOS). Das Projekt ist bewusst **plattformneutral** konzipiert — Linux-spezifische Konzepte gehören nicht in den Kern.

## Architekturprinzipien

- **Enterprise-Architektur**: klare Modulgrenzen, getrennte Verantwortlichkeiten, saubere Domänen- und Service-Schichten.
- **Schichtenmodell** (von innen nach außen):
  - `domain/` — reine Domänenobjekte (Inode, Volume, ACL, Snapshot, Metadata)
  - `storage/` — Persistenz, Blockallokation, Volume-Image, Katalog
  - `services/` — fachliche Services (Integrity, Recovery, Journal, Security, Versioning, Sync, Indexing)
  - `platform/` — **optionale** Plattformadapter (FUSE unter Linux, Performance-Tool, Runtime)
  - `app/` — Anwendungsorchestrierung
  - `cli.rs` / `config.rs` / `error.rs` — Einstiegspunkte und Querschnitt
- Plattformspezifische Abhängigkeiten (`fuser`, `libc`) sind ausschliesslich über `[target.'cfg(...)'.dependencies]` in `Cargo.toml` einzubinden.
- Fremdsysteme werden nur über optionale Kompatibilitäts- und Plattformadapter angebunden — niemals direkt im Kern.

## Entwicklungsregeln

- Sprache: **Rust**, Edition 2024.
- Struktur, Testbarkeit und Wartbarkeit haben Vorrang vor Feature-Vollständigkeit.
- Möglichst vollständige Testabdeckung für vorhandene Implementierung anstreben.
- Keine Abstraktion ohne konkreten Bedarf — keine spekulativen Generalisierungen.
- Keine plattformspezifischen Annahmen im `domain/`- oder `storage/`-Layer.

## Commit-Workflow

**Nach jeder Änderung gilt:** Laufen alle Tests erfolgreich durch (`cargo test`), wird unmittelbar ein Commit erstellt. Kein Commit bei fehlschlagenden Tests.

```bash
cargo test          # muss vollständig grün sein
git add <geänderte Dateien>
git commit -m "..."
```

## Tests ausführen

```bash
cargo test                        # alle Tests
cargo test <modulname>            # Tests eines Moduls
cargo test -- --nocapture         # mit stdout-Ausgabe
```

## Performance-Tool

Das Performance-Tool (`src/platform/performance.rs`, `src/platform/tools.rs`) wird kontinuierlich ausgebaut:
- Vordefinierte Suites: `dev`, `ci`, `regression`, `storage-heavy`
- Benchmark-Ergebnisse sollen automatisch mit früheren Messungen verglichen werden können.
- Messungen werden in `PERFORMANCE_LOG.md` festgehalten.

## Projektdokumentation

| Datei | Inhalt |
|---|---|
| `PROJECT_PROGRESS.md` | Zentrales Fortschritts-Tracking — bei jeder relevanten Änderung aktualisieren |
| `features_corefs.md` | Feature-Anforderungen |
| `corefs_brainstorming.txt` | Architektur- und Designüberlegungen |
| `PERFORMANCE_LOG.md` | Benchmark-Ergebnisse |

## Referenzdateien

Bei Erweiterungen sind `features_corefs.md` und `corefs_brainstorming.txt` zu konsultieren, um Anforderungen nicht zu übersehen.
