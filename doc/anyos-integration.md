# AnyOS-Integration

Dieses Dokument beschreibt, wie CoreFS als natives Dateisystem in **AnyOS** eingebunden ist. Es richtet sich an CoreFS-Entwickler, die Änderungen am Kern auf Kompatibilität mit dem AnyOS-Kernel prüfen müssen. Die umgekehrte Sicht (wie AnyOS CoreFS nutzt) findet sich in [anyos/docs/corefs.md](../../anyos/docs/corefs.md).

## Abhängigkeitsrichtung

AnyOS bindet CoreFS als **Pfad-Dependency** ein:

```toml
# anyos/kernel/Cargo.toml
corefs-core = { path = "../../corefs/corefs-core", default-features = false }
```

Der AnyOS-Kernel konsumiert ausschließlich das `corefs-core`-Crate, niemals die FUSE-Adapter oder die Linux-CLI. `default-features = false` deaktiviert das `crypto`-Feature, da Poly1305-SIMD-Intrinsics auf dem Soft-Float-Kernel-Target `x86_64-anyos` nicht verfügbar sind. CoreFS muss daher sauber in der Konfiguration **ohne** `std`- und `crypto`-Feature bauen — nur mit `alloc`.

## Anforderungen an `corefs-core`

Damit der Kernel-Konsum stabil bleibt:

- **`no_std + alloc`** — alle für AnyOS relevanten Module (`domain`, `storage`, `services`, `platform`) dürfen nicht implizit `std` verlangen. `#![cfg_attr(not(feature = "std"), no_std)]` in `lib.rs` bleibt verbindlich.
- **Keine `SystemTime`-Abhängigkeiten** in Code-Pfaden, die ohne `std` erreichbar sind. Zeitstempel kommen über den `platform::Clock`-Trait.
- **Keine `getrandom`/OS-RNG-Aufrufe** in no_std-Pfaden. Zufall kommt über den `platform::Rng`-Trait.
- **Kryptografie optional**. Alles hinter `cfg(feature = "crypto")`. Verschlüsselung-at-Rest ist im AnyOS-Kernel aktuell nicht aktiv.

Neue Features im Kern sind entsprechend einzuordnen: Platform-Trait-basiert, crypto-gated, oder std-only (dann nur für FUSE/CLI nutzbar).

## Vom Kernel bereitgestellte Plattform-Implementierungen

AnyOS liefert die `platform`-Traits selbst (`anyos/kernel/src/fs/corefs/mod.rs`):

| Trait | Kernel-Implementierung | Hinweise |
|---|---|---|
| `platform::Clock` | `KernelClock` | xorshift64-basiert, CSPRNG-Nonce als Platzhalter |
| `platform::Rng` | `KernelRng` | xorshift64-Generator |
| `storage::block_device::BlockDevice` | `BlockDeviceAdapter` | byte→Sektor-Mapping (512 B), Partition-Offset |

Erweiterungen dieser Traits in CoreFS erzwingen parallele Anpassungen im AnyOS-Kernel. Solche Änderungen sollten im `PROJECT_PROGRESS.md` als Breaking-Change markiert werden.

## Vom Kernel genutzte API-Oberfläche

Der Treiber in [`anyos/kernel/src/fs/corefs/driver.rs`](../../anyos/kernel/src/fs/corefs/driver.rs) greift im Wesentlichen auf:

- `storage::persisted_state::{PersistedState, load_state_native, save_state_native}` — Hydration und Flush
- `storage::ondisk::*` — Superblock-Magic (`ODF_MAGIC`), Reader, FSCK, Resize, Scrub, Tier, Volume-Format
- `domain::*` — Inode, Metadata, Verzeichnisstrukturen
- `CoreFsError` → VFS-Fehlerabbildung über `corefs_to_fs_error()`

Umbenennungen oder Signatur-Änderungen an diesen Einstiegspunkten brechen den Kernel-Build.

## Userland-Tools (AnyOS)

Die CLI-Tools unter `anyos/bin/` (mkfs.corefs, fsck.corefs, corefs-dump, corefs-tier, corefs-snapshot, corefs-resize, corefs-defrag, corefs-scrub) verwenden `corefs-core` **mit** `crypto`-Feature, da sie gegen das User-Target `x86_64-anyos-user` gebaut werden. Sie greifen direkt auf `corefs_core::storage::ondisk::*` zu und teilen sich die Abstraktion `libcorefs-tools` (Block-Device-Adapter, Argumentparser, Report-Renderer, Exit-Code-Mapping).

## Prüfungen vor einem Release

Vor dem Tag eines neuen CoreFS-Releases:

1. `cargo build --no-default-features --features alloc -p corefs-core` muss erfolgreich sein.
2. Im AnyOS-Repo `cargo build -p kernel --target x86_64-anyos.json` ausführen.
3. Alle `anyos/bin/corefs-*`- und `mkfs.corefs`/`fsck.corefs`-Tools müssen ohne API-Anpassung bauen — sonst ist ein koordiniertes Update nötig.

## Weiterführend

- [architecture.md](architecture.md) — Schichtenmodell, das die Trennung zwischen Kern und Plattform erzwingt
- [persistence-format.md](persistence-format.md) — On-Disk-Format, das Kernel-Probe und Userland-Tools gemeinsam lesen
- [anyos/docs/corefs.md](../../anyos/docs/corefs.md) — Integrationsdoku aus Sicht von AnyOS
