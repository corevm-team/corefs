# Deduplizierung

CoreFS dedupliziert Blockdaten **inline beim Schreiben** und bietet zusätzlich einen **expliziten Pass** zur Konsolidierung und Konsistenzprüfung. Kern der Implementierung ist der `BlockStore` in [corefs-core/src/storage/block_store.rs](../corefs-core/src/storage/block_store.rs).

## Datenmodell

Der `BlockStore` trennt **logische Blöcke** von **physischen Blobs**:

- `blocks: HashMap<InodeId, BlockEntry>` — logische Sicht pro Inode. Ein `BlockEntry` speichert u. a. den Device-Block, die Grösse und den **`blob_checksum`** als Referenz auf den Inhalt.
- `blobs: HashMap<u64, BlobRecord>` — physische Sicht. Ein `BlobRecord` enthält die Bytes, ihre Checksumme und einen **`ref_count`**.

Zwei Inodes mit byte-identischem Inhalt teilen denselben `BlobRecord`; es existiert nur eine physische Kopie, der `ref_count` zählt die Referenzen.

## Inline-Deduplizierung beim Schreiben

Beim Schreiben eines Blocks ([block_store.rs:192](../corefs-core/src/storage/block_store.rs#L192) ff.):

1. Die Bytes werden mit einer FNV-ähnlichen 64-Bit-Checksumme gehasht (`checksum()` in [block_store.rs:1003](../corefs-core/src/storage/block_store.rs#L1003)).
2. `self.blobs.entry(checksum).or_insert_with(...)` ([block_store.rs:238](../corefs-core/src/storage/block_store.rs#L238)) sucht den Blob in der Hashmap:
   - **Treffer** → vorhandener `BlobRecord` wird wiederverwendet, `ref_count += 1`, keine neuen Bytes werden gespeichert.
   - **Kein Treffer** → neuer Blob wird angelegt, `ref_count = 1`.
3. Der `BlockEntry` des schreibenden Inodes wird auf `blob_checksum = checksum` gesetzt.

Wird ein existierender Block überschrieben, dekrementiert der Store zuerst den `ref_count` des alten Blobs und entfernt ihn, sobald er auf 0 fällt ([block_store.rs:210](../corefs-core/src/storage/block_store.rs#L210), [block_store.rs:973](../corefs-core/src/storage/block_store.rs#L973)). So bleibt CoW, Dedup und Referenzzählung in einem einzigen Pfad konsistent.

Für Append-Schreibvorgänge nutzt der Store eine **inkrementelle Checksumme** (`checksum(A ++ B) = checksum(B, seed = checksum(A))`, [block_store.rs:294](../corefs-core/src/storage/block_store.rs#L294) ff.), sodass angehängte Daten ohne vollständige Rehash-Operation auf einen neuen Blob umgestellt werden können.

## Klonen ohne Byte-Kopie

Beim Klonen eines Inodes ([block_store.rs:474](../corefs-core/src/storage/block_store.rs#L474) ff.) wird lediglich ein neuer `BlockEntry` angelegt, der auf denselben `blob_checksum` zeigt; der `ref_count` des Blobs wird erhöht. Dadurch sind `clone` und Snapshot-Capture O(1) pro Datei und verbrauchen keinen zusätzlichen Speicherplatz, bis einer der Beteiligten beschreibt (CoW).

## Expliziter Dedup-Pass

`BlockStore::dedup_pass()` ([block_store.rs:533](../corefs-core/src/storage/block_store.rs#L533)) kann jederzeit — z. B. aus `fsck`, Wartungsjobs oder Tests — aufgerufen werden und läuft in drei Phasen:

### Phase 1 — Ref-Count-Audit

Der Store zählt pro `blob_checksum`, wie viele `BlockEntry`-Records tatsächlich darauf verweisen, und korrigiert abweichende `ref_count`-Werte. Verwaiste Blobs (`ref_count == 0` ohne Referenzen) werden entfernt.

### Phase 2 — Hash-Kollisionserkennung

Für jeden Blob wird `checksum(blob.bytes)` neu berechnet und mit dem gespeicherten Schlüssel verglichen. Jede Abweichung zählt als Kollision und wird im Report ausgewiesen — ein Wert > 0 signalisiert ein Datenintegritätsrisiko (z. B. Korruption oder Hash-Algorithmus-Wechsel).

### Phase 3 — Byte-identische Konsolidierung

Blobs werden nach ihrem tatsächlichen Byte-Inhalt gruppiert. Finden sich byte-gleiche Blobs unter verschiedenen Checksummen, wird ein **kanonischer Blob** gewählt; alle übrigen werden entfernt, ihre Ref-Counts auf den kanonischen addiert und alle `BlockEntry`-Referenzen umgelenkt.

Der zurückgegebene `DedupePassReport` ([block_store.rs:118](../corefs-core/src/storage/block_store.rs#L118)) enthält:

| Feld | Bedeutung |
|---|---|
| `blobs_scanned` | Anzahl geprüfter Blobs |
| `bytes_scanned` | Summierte Blob-Grösse |
| `duplicates_consolidated` | Zahl zusammengeführter Duplikate |
| `bytes_reclaimed` | Durch Konsolidierung freigegebene Bytes |
| `hash_collisions` | Blobs mit falscher Checksumme |
| `ref_count_mismatches` | Korrigierte Ref-Count-Abweichungen |

## Statistik

`BlockStore::dedupe_stats()` ([block_store.rs:612](../corefs-core/src/storage/block_store.rs#L612)) liefert ein kompaktes Laufzeit-Mass:

- `logical_blocks` — Anzahl der `BlockEntry`-Einträge
- `unique_blobs` — Anzahl der physisch gespeicherten Blobs
- `deduplicated_blocks` = `logical_blocks − unique_blobs` — eingesparte physische Blöcke

## Grenzen und Ausblick

- Die aktuelle 64-Bit-Checksumme ist schnell, aber **nicht kryptographisch**. Die Hash-Kollisionserkennung in Phase 2 des Dedup-Passes deckt zufällige Kollisionen auf; für adversarielle Szenarien ist ein Umstieg auf einen starken Hash (z. B. BLAKE3) vorgesehen.
- Dedup arbeitet auf **Block-Ebene**, nicht auf variabler Chunk-Grenze. Inhaltsverschobene, aber identische Payloads werden nicht zusammengeführt.
- Der Dedup-Pass ist im Backend synchron; für sehr grosse Volumes wäre eine inkrementelle/hintergrundbasierte Variante ein sinnvoller nächster Schritt.

## Tests

Das Verhalten ist in [corefs-core/src/storage/block_store_tests.rs](../corefs-core/src/storage/block_store_tests.rs) abgesichert (Inline-Dedup, Ref-Counting, Klonen, Dedup-Pass mit Kollisions- und Konsolidierungsfällen).
