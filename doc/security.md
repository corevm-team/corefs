# Sicherheit

Dieser Abschnitt beschreibt Verschlüsselung, Zugriffskontrolle und sicheres Löschen. Ergänzend: [key-management.md](key-management.md).

## Verschlüsselung ruhender Daten ✅

- **Algorithmus**: ChaCha20-Poly1305 (AEAD, 256-Bit-Key, 96-Bit-Nonce, 128-Bit-Tag).
- **Implementierung**: `corefs-core/src/services/encryption.rs` (no_std-fähig, feature-gate `crypto`) und `src/services/encryption.rs` (std-Variante).
- **Pipeline beim Schreiben**: `compress → encrypt → store`. Beim Lesen umgekehrt.
- **Ciphertext-Layout**: `nonce[12] || ciphertext || tag[16]`.
- **Per-File-Keys**: abgeleitet via **HKDF-SHA256** aus dem Master-Key und der `InodeId`. Pure-Rust-Implementierung (RFC-5869, FIPS-180-4-Tests).
- **Steuerung**: Flag `FileMetadata.encrypted` pro Inode, Config-Schalter `config.security.encryption_at_rest`.

Tamper-Detection erfolgt automatisch durch den AEAD-Tag — jedes Bit-Kippen im Ciphertext schlägt beim Entschlüsseln fehl und produziert einen Fehler im FUSE-Pfad.

## Keystore & Master-Key ✅

- Dateiformat mit Magic `"COREFSKS"`.
- Master-Key wird **unter einer Passwort-KDF AEAD-verpackt** abgelegt.
- Rotation des Master-Keys ist **ohne Re-Encryption** möglich — HKDF-Ableitung bleibt stabil, solange die `InodeId` unverändert ist.

CLI:

```bash
corefs keys init      ./keystore.bin   # Neu erstellen
corefs keys rotate    ./keystore.bin   # Master-Key rotieren
corefs keys verify    ./keystore.bin   # Format/Integrität prüfen
```

Offener Punkt (🔶): Die Passwort-KDF verwendet heute eine leichtgewichtige Ableitung, für den Produktiveinsatz ist **Argon2id** als Standard vorgesehen — der Code-Hook ist vorbereitet, der Algorithmus noch nicht verdrahtet.

## Zugriffskontrolle

### POSIX-Mode ✅

`uid`, `gid`, `mode` werden on-disk pro Inode gespeichert und beim `getattr`/`setattr` korrekt transportiert. `chown`/`chmod` funktionieren im FUSE-Mount.

### ACLs 🔶

- Typen in `corefs-core/src/domain/acl.rs`: `AclEntry { principal, permissions }`, `Principal::{Unix(u32), Group(u32), Other, Extended(String)}`.
- Speicherung ist vollständig.
- **Enforcement**: Der Linux-FUSE-Pfad prüft heute **nur** die POSIX-Mode-Bits, nicht die erweiterten ACL-Einträge. Für strikte POSIX-ACL-Semantik muss der Entscheidungspfad in `linux_fuse.rs` um ACL-Evaluation erweitert werden.

### xattr ✅ (Speicherung) / 🔶 (FUSE-Routing)

- Domain-Modell: `FileMetadata.xattrs: BTreeMap<String, Vec<u8>>`.
- On-Disk: separater `xattr_block`.
- FUSE: noch kein vollständiges Routing der vier xattr-Ops (`getxattr`, `setxattr`, `listxattr`, `removexattr`) in die Service-Fassade.

## Secure-Delete ✅

- `delete --secure` (CLI) bzw. `CoreFsService::delete_file(path, secure=true)` überschreibt alle referenzierten Blöcke vor der Freigabe.
- CoW-Shared-Blöcke werden nur freigegeben, wenn der Ref-Count auf Null fällt — ansonsten wird das Secure-Erase auf die letzte Referenz verschoben.
- Integration mit `TRIM`/`BLKDISCARD` auf Blockgeräten.

## Quotas ✅

- `QuotaService` prüft `max_files` und `max_bytes` aus `config.persistence` bei jeder Create- und Write-Operation.
- Überschreitung führt zu `CoreFsError::QuotaExceeded` → FUSE-`ENOSPC`.

## Angriffsmodell & Grenzen

- **Offline-Angreifer auf Image/Device**: Ohne Master-Key sind AEAD-verschlüsselte Blobs nicht lesbar. Die **Metadaten** (Pfade, Grösse, Mode, Timestamps, Tags) sind aktuell **nicht verschlüsselt** — es gibt keinen "encrypt-metadata"-Modus.
- **Online-Angreifer mit Root**: POSIX-Mode und ACLs werden ausschliesslich im Userland-FUSE geprüft. Ein Kernel-Root kann das Image direkt lesen.
- **Seitenkanäle**: Keine konstante-Zeit-Garantien jenseits der Krypto-Library. Keine Schutzmassnahmen gegen Timing-Angriffe auf Pfadauflösung.

## Offene Punkte / Verbesserungsbedarf

| Thema | Status | Empfehlung |
|---|---|---|
| Argon2id als Password-KDF | 🔶 | Verdrahten, Kompatibilitätsflag im Keystore ergänzen |
| ACL-Enforcement im FUSE-Pfad | 🔶 | `check_access(inode, uid, op)` in `linux_fuse.rs` einführen |
| Metadaten-Verschlüsselung | ⚠️ | separater Opt-In-Modus, eigener Catalog-Blob |
| xattr-End-to-End | 🔶 | vier xattr-Ops durch die Service-Fassade routen |
| Audit-Log / Security-Journal | ⚠️ | Hook im Journal-Service, bislang keine Signatur |
| Key-Escrow / HSM-Integration | ⚠️ | aktuell keine externe Key-Quelle |
