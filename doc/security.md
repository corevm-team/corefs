# Sicherheit

## Verschlüsselung ruhender Daten

Implementierung: [src/services/encryption.rs](../src/services/encryption.rs) (~6.7 KB).

- **Algorithmus**: ChaCha20-Poly1305 (AEAD)
- **Schlüssellänge**: 256 Bit
- **Nonce**: 12 Byte, pro Datei/Block frisch
- **Schlüsselableitung**: `derive_key_from(secret, salt)`

### Aktivierung

Über [`SecurityPolicy`](configuration.md):

```rust
SecurityPolicy { encryption_at_rest: true, .. }
```

Bei aktiver Policy:
- `write_file()` verschlüsselt Bytes transparent vor dem Speichern.
- `read_file()` entschlüsselt transparent.
- `FileMetadata::encrypted = true` wird gesetzt.
- FUSE-Mounts (ro und rw) arbeiten mit entschlüsselten Daten.

### Tamper-Detection

Das AEAD-Tag authentifiziert die verschlüsselten Daten. Manipulation erkannt → `CoreFsError::State` beim Read. Siehe auch `services::security` für kombinierte Tamper-Erkennung über Checksummen + AEAD.

## ACLs

Implementierung: [src/domain/acl.rs](../src/domain/acl.rs).

```rust
pub enum Principal {
    User(String),
    Group(String),
    Other,
}

pub struct AclEntry {
    pub principal: Principal,
    pub permissions: Permissions,
}
```

Jede Datei trägt eine Liste `AclEntry`. Zugriffsprüfung durch `services::security`.

## Quotas

Implementierung: [src/services/quota.rs](../src/services/quota.rs) (~5.2 KB).

```rust
QuotaPolicy {
    max_files: Some(10_000),
    max_bytes: Some(10 * 1024 * 1024 * 1024),
}
```

Enforcement:
- `create_file()` → prüft `max_files`
- `write_file()` → prüft `max_bytes`
- Überschreitung → `CoreFsError::PolicyViolation`

## Secure-Delete

```bash
cargo run -- delete /secret.txt --secure
```

Überschreibt alle zugehörigen Blöcke mit Null-Bytes, entfernt Inode aus Catalog und verhindert `restore_file()`. Erst nach dem nächsten `flush` / `save-image` ist die Aktion persistent auf Disk.

## Empfehlungen

- Immer beide Schichten kombinieren: **Encryption at rest** + **ACLs**.
- Schlüsselmaterial nicht im Klartext im Image ablegen.
- Für Blockgeräte-Setups zusätzlich externe Schlüsselverwaltung (HSM, Keyring) vorsehen.
- Secure-Delete vor `save-image` ausführen, damit der überschriebene Zustand tatsächlich persistiert wird.
