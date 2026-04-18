# Produktives Key-Management

CoreFS trennt zwischen **Master-Key** (außerhalb des Volumes, unter Kontrolle
des Operators) und **Volume-Key** (im Keystore, AEAD-gewrappt). Per-File-Keys
werden deterministisch aus dem Volume-Key abgeleitet.

## Architektur

```
     Master-Key (32 B, from Operator)
         │
         │  ChaCha20-Poly1305 AEAD
         ▼
   Keystore-Datei  ──▶  entsiegelt  ──▶  Volume-Key (32 B)
   (wrapped, on disk)                        │
                                             │  HKDF-SHA256
                                             ▼
                                    Per-File-Key (InodeId)
```

Layer:
- **Kern:** `corefs_core::security::{sha256, hkdf, keystore}` (no_std + alloc,
  feature-gated unter `crypto`, keine externen Crypto-Deps außer
  `chacha20poly1305`)
- **Host-Tool:** `corefs_tools::keys`
- **CLI:** `corefs-cli keys init|rotate|verify`

## Kernel-Primitive

### SHA-256 (`security::sha256`)

Pure-Rust FIPS-180-4-Implementierung. Enthält:
- `Sha256` — inkrementeller Hasher (`new`, `update`, `finalize`)
- `sha256(data)` — One-Shot
- `hmac_sha256(key, data)` — HMAC-SHA256 für HKDF
- Verifiziert gegen NIST-Testvektoren ("", "abc") und
  RFC 4231 HMAC Test Case 1.

### HKDF-SHA256 (`security::hkdf`)

RFC 5869 Implementierung:
- `extract(salt, ikm) → PRK`
- `expand(prk, info, len) → OKM`
- `derive(salt, ikm, info, len)` (Shortcut)
- Verifiziert gegen RFC 5869 Test Cases 1 und 3.

### Keystore (`security::keystore`)

| API | Zweck |
|-----|-------|
| `Keystore::new(volume_key, salt, volume_uuid)` | In-Memory-Keystore |
| `derive_file_key(InodeId)` | HKDF → 32-Byte Per-File-Key |
| `wrap(master_key, nonce)` | AEAD-Wrap des Volume-Keys |
| `unwrap_volume_key(master_key, wrapped)` | Reverse |
| `rotate_master(old, new, wrapped, nonce)` | Re-wrap, Volume-Key unverändert |
| `export_file(master_key, nonce, ts)` | `KeystoreFile`-Struct |
| `import_file(&KeystoreFile, master_key)` | Inverse |

Per-File-Key-Ableitung:

```
info  := "corefs-per-file-key-v1" || BE64(inode_id)
PRK   := HMAC-SHA256(salt, volume_key)
key   := HKDF-Expand(PRK, info, 32)
```

## KeystoreFile (Wire-Format)

| Feld                | Typ         |
|---------------------|-------------|
| `magic`             | `u64` = `"COREFSKS"` LE |
| `version`           | `u16` (aktuell `1`) |
| `kdf`               | `KdfConfig { algorithm, salt: [u8;32], file_info }` |
| `wrapped_volume_key`| `Vec<u8>` = `nonce (12) || ct || tag (16)` |
| `volume_uuid`       | `[u8; 16]` |
| `created_at`        | `Timestamp` |

Serialisiert als `bincode`-Legacy (dieselbe Pipeline wie Volume-Image
und Backup-Stream).

## CLI

```bash
# Master-Key aus /dev/urandom (muss der Operator sicher aufbewahren!)
head -c 32 /dev/urandom > ~/.corefs/keys/master.bin
chmod 600 ~/.corefs/keys/master.bin

# Neuen Keystore anlegen
corefs-cli keys init ~/.corefs/keys/vol1.kst \
    --master-key ~/.corefs/keys/master.bin \
    --volume-uuid 0102030405060708090a0b0c0d0e0f10 \
    --json

# Master-Key rotieren
head -c 32 /dev/urandom > ~/.corefs/keys/master.new
corefs-cli keys rotate ~/.corefs/keys/vol1.kst \
    --old-master ~/.corefs/keys/master.bin \
    --new-master ~/.corefs/keys/master.new
mv ~/.corefs/keys/master.new ~/.corefs/keys/master.bin

# Integritaet pruefen (Magic + Version + Probe-Unwrap)
corefs-cli keys verify ~/.corefs/keys/vol1.kst \
    --master-key ~/.corefs/keys/master.bin
```

## Rotation

Rotation ändert **ausschließlich** das Wrapping des Volume-Keys unter dem
Master-Key. Der Volume-Key selbst bleibt stabil, daher:

- Alle Per-File-Keys bleiben identisch.
- Keine Datei muss umverschlüsselt werden.
- Betriebliche Rotation des Master-Keys (z. B. quartalsweise) ist
  hochperformant und ohne Datenbewegung möglich.

Ein **Volume-Key-Rotate** (mit Re-Encryption aller Dateien) ist separat —
aktuell nicht in diesem Tool; die Online-Re-Encryption ist eine spätere
Erweiterung.

## Integritaet

- Magic-Check (`COREFSKS`) verhindert Fremddateien.
- Version-Check vermeidet stumme Wire-Format-Änderungen.
- AEAD-Tag-Prüfung beim Unwrap erkennt Tampering und falsche Keys
  (liefert `PolicyViolation`, nicht `State` — vermeidet
  oracle-Artefakte).
- `Debug`-Impl redaktiert den Volume-Key (`"<redacted>"`).

## Tests

**Kern** (`corefs-core/src/security/`):
- SHA-256: NIST-Vektoren + incremental-match
- HMAC-SHA256: RFC 4231 Test Case 1
- HKDF: RFC 5869 Test Cases 1 + 3
- Keystore: 15 Tests — determinismus, per-inode-/per-salt-varianz,
  wrap/unwrap, wrong-master, rotation-preserves, magic/version-reject,
  tamper-detection, wire-format-stability

**Host** (`corefs-tools/src/keys_tests.rs`): 8 Tests —
init-creates-valid, verify ok/bad, rotate-roundtrip, bad-uuid-reject,
bad-master-size-reject, metadata-JSON, double-init-overwrites.

## Bekannte Limits

- Master-Key kommt aktuell als **32-Byte-Rohdatei**. Passphrase-KDF
  (Argon2) ist noch nicht im Tool — das ist bewusst, weil die Argon2-
  Dependency ein separater Schritt ist. Operatoren, die Passphrasen
  brauchen, laufen sie vorab durch `argon2` CLI und legen das
  32-Byte-Output in der Keystore-Datei ab.
- Keine TPM/HSM-Integration — das ist AnyOS-spezifisch und wird
  separat geplant.
- Volume-Key-Rotate (mit File-Re-Encryption) ist nicht implementiert;
  nur Master-Key-Rotate.
- EncryptionService-Integration (automatisches File-Key-Binding) ist
  noch ein separater Migrationsschritt.
