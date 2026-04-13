# Konfiguration

Die zentrale Konfiguration ist in [src/config.rs](../src/config.rs) definiert.

## `CoreFsConfig`

```rust
pub struct CoreFsConfig {
    pub volume_name: String,
    pub block_size: usize,
    pub inode_table_capacity: usize,
    pub default_tier: StorageTier,
    pub versioning: VersioningPolicy,
    pub security: SecurityPolicy,
    pub performance: PerformancePolicy,
    pub quotas: QuotaPolicy,
}
```

## Policies

### `VersioningPolicy`

```rust
VersioningPolicy {
    keep_latest: 16,
    auto_prune: true,
    expose_time_travel: true,
    max_version_bytes: Some(64 * 1024 * 1024), // 64 MiB
}
```

- `keep_latest` — Mindestanzahl behaltener Versionen pro Datei
- `auto_prune` — Versionen automatisch löschen, wenn Budget überschritten
- `expose_time_travel` — aktiviert `@`-Syntax in FUSE-Mounts
- `max_version_bytes` — globales Byte-Budget für Versions-Historie

### `SecurityPolicy`

```rust
SecurityPolicy {
    encryption_at_rest: true,
    acl_enabled: true,
    secure_delete_supported: true,
}
```

### `PerformancePolicy`

```rust
PerformancePolicy {
    journaling_enabled: true,
    copy_on_write: true,
    compression_enabled: true,
    deduplication_enabled: false,
    trim_enabled: true,
}
```

### `QuotaPolicy`

```rust
QuotaPolicy {
    max_files: None,   // Option<u64>
    max_bytes: None,   // Option<u64>
}
```

Wenn gesetzt, wird das Limit bei `create_file` / `write_file` über den `QuotaService` durchgesetzt und liefert `CoreFsError::PolicyViolation`.

## `StorageTier`

Aufzählung: `Hot | Warm | Cold`. Standard: `Hot`. Tiering ist konzeptionell vorgesehen, aber noch nicht vollständig aktiv (siehe [features.md](features.md)).

## Programmgesteuerte Nutzung

```rust
use corefs::{CoreFsConfig, CoreFsService};

let config = CoreFsConfig::default();
let mut fs = CoreFsService::mkfs(config)?;
fs.write_file("/a.txt", b"hallo")?;
```
