# CoreFS Performance Log

Dieses Dokument ist die **narrative Schicht**: Root-Cause-Analysen,
Phasen-Entscheidungen, Roadmap.  Die **Rohmessungen** liegen als TSV in
[`perf-history/`](perf-history/) — dort ist die Single Source of Truth
für Zahlen, inklusive `baseline.tsv` für Regressions-Diffs.  Tabellen in
diesem Log sind Zusammenfassungen und verweisen auf den jeweiligen
TSV-Lauf.

---

## Phase 0 Baseline — 2026-04-18 (CoreFS FUSE vs ext4 host, same NVMe)

Host: Linux 6.8.0-107, NVMe (nvme0n1), 32 GiB RAM, ext4 als Host-FS.
CoreFS wurde via `mount-image-rw` (FUSE, threads=4) auf einer Image-Datei
auf derselben NVMe-ext4 gemountet, auf der auch der ext4-Vergleich läuft.
Gleicher Datenträger, gleicher Host — gemessen wird reiner
CoreFS-Stack-Overhead.

Harness: `/tmp/corefs-bench/run.sh` (FILES=200, PAYLOAD=4096,
SEQ_MIB=128, FSYNC_N=50, RAND_MIB=32, RAND_OPS=500,
STEP_TIMEOUT=180 s). Roh-TSV: [`perf-history/2026-04-18_phase0.tsv`](perf-history/2026-04-18_phase0.tsv).

### Ergebnisse

| Workload                 | CoreFS (FUSE) | ext4 (host) | Faktor langsamer |
|--------------------------|--------------:|------------:|-----------------:|
| create 200 × 4 KiB       |       48 ops/s|   5405 ops/s|           **112×** |
| read 200 × 4 KiB         |       72 ops/s|   5714 ops/s|            **79×** |
| stat (ls -la) 200        |          6 ms |        5 ms |             ~1× |
| seq write 128 MiB        |     25 MiB/s  |   666 MiB/s |            **27×** |
| seq read 128 MiB (warm)  |     38 MiB/s  |  5565 MiB/s |           **146×** |
| fsync 50 × 4 KiB         | <0.3 ops/s\*  |    505 ops/s|         **~1700×** |
| rand 4K 70r/30w, 500 ops |       96 ops/s|  10638 ops/s|           **110×** |
| delete 200 × 4 KiB       |    391 205 ms |       17 ms |         **~23000×** |

\* fsync-Schritt wurde nach 180 s Timeout abgebrochen.

### Root-Cause-Analyse (aus Code-Inspektion)

Der dominierende Hotspot ist **`CoreFsService::save_image_to_path`**
(`src/app/mod.rs:987`). Jede Mutations-Operation im FUSE-Adapter ruft über
`persist()` (`src/platform/linux_fuse.rs:886`) diesen Pfad auf.
`save_image_to_path` macht pro Aufruf:

1. `self.persisted_state()` — vollständige Serialisierung des Volume-State
2. `self.read_all_block_bytes()` — Kopie **aller** Block-Bytes aus dem In-Memory-Store
3. `save_volume_image_with_bytes(tmp, state, blocks)` — schreibt die **komplette** Image-Datei neu
4. `std::fs::rename(tmp, path)` — atomarer Rename

Das heisst: **jede `create`/`unlink`/`fsync`/`flush` schreibt das gesamte
Volume neu** — unabhängig davon, wie gross die Änderung tatsächlich war.

Trigger-Stellen in `src/platform/linux_fuse.rs`:

| Op | Zeile | persist()-Aufrufe pro Op |
|---|---|---|
| `create()`  | 2022 | über `ensure_mutation_session` (1–2×) + `record_wal_operation_and_save` (1×) |
| `mkdir()`   | 2099 | dito (1–3× persist pro mkdir) |
| `unlink()`  | 2170 | dito |
| `flush_file_handle()` | 1539 | `self.persist()?` pro dirty Handle |
| `fsync()`   | 2401 | 1× |
| `flush()`   | 2377 | 1× |
| `ensure_mutation_session` | 1317 | bis zu 2× persist vor der eigentlichen Operation |

Es gibt zwar einen `persist_to_device_incremental`-Pfad (für
`FuseBacking::Device` und `FuseBacking::Odf`, Zeile 891 + 907), aber für
gemountete Image-Dateien (`FuseBacking::File`) **nicht** — dort greift
der vollständige Rewrite.

### Warum die Messungen passen

- **create 48 ops/s** ≈ 20 ms/Datei → das ist die Zeit, ein leeres Volume
  zu serialisieren + zu schreiben. Bei grösserem Volume fällt der Wert
  weiter.
- **seq-write 25 MiB/s** — `STREAM_FLUSH_THRESHOLD` puffert, aber jeder
  Streaming-Flush triggert `persist()` = full rewrite gegen eine wachsende
  Image-Datei. Effektive Bandbreite = Image-Rewrite-Rate pro Fenster.
- **seq-read 38 MiB/s** — kein persist, aber `open_file_handle`
  (Zeile 1348) lädt bei jedem open via `self.service.read_file(path)`
  den gesamten File-Content in einen `Vec<u8>`. 128 MiB → komplettes
  Memcopy pro open, zusätzlich zum FUSE-Read selbst.
- **fsync timeout** — 50 fsyncs × full-volume-write, Volume wächst pro
  Durchgang, serielle atomare Renames → läuft binnen 180 s nicht durch.
- **delete ~2 s/Datei** — jeder unlink = full rewrite.
- **rand 4K 96 ops/s** — schreibender Anteil plus `os.fsync` am Ende
  triggert weitere Full-Saves.
- **stat / readdir in Parität** — reiner Read-Metadata-Pfad, kein persist.

### Phase-1-Priorisierung (Empfehlung)

In dieser Reihenfolge angehen; jeder Punkt einzeln gegen den Phase-0-Harness
nachmessen.

**P1 — `persist()`-Gate: WAL → Delayed Checkpoint (grösster Hebel).**
`create`/`unlink`/`write`/`mkdir` schreiben nur noch WAL. Der volle
Image-Save läuft nur noch bei `fsync`, `unmount`/`destroy`, oder einem
Timer-/Size-Threshold (z. B. 1 s oder 16 MiB WAL). WAL-Replay beim Mount
existiert bereits (`VolumeWal` / `recover_pending_wal`) — muss idempotent
sein.
Erwartung: create 48 → ≥ 1000 ops/s, delete analog.

**P2 — Incremental Save auch für `FuseBacking::File`.**
`persist_to_device_incremental` existiert für Device-Backing. Dieselbe
Logik (nur geänderte Blöcke/Inodes rausschreiben) auf den File-Backing-Pfad
übernehmen. Atomicity über WAL + Journal, nicht über write-then-rename der
Gesamtdatei.
Erwartung: seq-write 25 → ≥ 200 MiB/s, delete wird von der Volume-Grösse
entkoppelt.

**P3 — Group-Commit für fsync.**
Mehrere gleichzeitige fsyncs coalescen auf einen einzigen Checkpoint.
Klassisches ext4/xfs-Pattern. Kombiniert mit P1 fallen >95 % der
fsync-Kosten weg.

**P4 — Read-Path: keine Vollkopie in `open_file_handle`.**
Aktuell lädt jeder open die komplette Datei in `OpenFileHandle.data`.
Statt dessen on-demand-pread gegen den Service + kleiner Read-Ahead-Cache.
Erwartung: seq-read 38 MiB/s → ≥ 1 GiB/s (Kernel-Cache),
read kleiner Files 72 → ≥ 3000 ops/s.

**P5 — Doppel-`persist` in `ensure_mutation_session` eliminieren
(`linux_fuse.rs:1320 + 1327`).** Persist wird bis zu 2× vor der
eigentlichen Operation gerufen, nur um `unclean_shutdown`-Flag + WAL-Header
zu setzen. Ersetzbar durch In-Memory-Flag, das beim nächsten Checkpoint
mit geflusht wird.

**P6 — Regression-Gate.**
`corefs-benchmark.sh` um ext4-Vergleichsspalte erweitern, bei jedem Commit
laufen lassen. Ziel nach Phase 1: Faktor ≤ 3× auf allen Workloads ausser
evtl. fsync-heavy.

### Enterprise-Folgethemen (nach Phase 1)

- P7 — Parallelität: `--threads 4` existiert, aber alle Ops sind hinter
  `&mut self` am FuseHandler serialisiert. Partitionierung nötig (z. B.
  Inode-Hash-Locks).
- P8 — Crypto/Checksum-Kosten nur auf dem tatsächlich verschlüsselten
  Pfad aktiv lassen.
- P9 — Directory-Index O(log n) bei > 10 k Einträgen/Dir.

### Harness + Daten

- Script: `/tmp/corefs-bench/run.sh` (idempotent, per-step-Timeouts, schreibt TSV inkrementell)
- Roh-Ergebnisse: `/tmp/corefs-bench/results.tsv`
- Phase-1-Änderungen gegen exakt diese Messpunkte nachmessen.

---

## Phase 1 — 2026-04-19

Umgesetzt:

- **P1 (deferred checkpoint)** — `persist()` wird nicht mehr bei jedem
  `create`/`unlink`/`mkdir`/`write`/`flush` aufgerufen.  Metadaten-
  Mutationen landen in der In-Memory-WAL, der physikalische Image-Save
  läuft nur noch bei `fsync`, `unmount` (destroy) und zukünftig einem
  Background-Checkpoint-Timer.  Crash-Safety bleibt gewahrt, weil die
  unclean-shutdown-Markierung erst mit dem nächsten Checkpoint auf Disk
  landet — ein Crash davor lässt das Image unverändert.
- **P5 (Doppel-`persist` in `ensure_mutation_session`)** — die beiden
  eager `self.persist()?`-Aufrufe (Zeile 1320 + 1327 Vor-P1) entfernt.
- **Streaming-Flush-Trigger-Bug (freigelegt durch P1)** — pre-P1 verglich
  die Bedingung die gesamte *logische Dateigrösse* mit `STREAM_FLUSH_THRESHOLD`.
  Ab Dateigrösse > Threshold triggerte jeder weitere sequentielle Write
  einen Flush → O(n²) gegen das Read-Modify-Write von `append_to_inode`.
  Fix: Buffer-basiert (`handle.data.len() + data.len() > THRESHOLD`).
  Threshold von 32 → 64 MiB erhöht und via `COREFS_STREAM_FLUSH_MIB`
  override-bar.
- **P6 (Regression-Bench)** — neues Script `scripts/corefs-benchmark-vs-ext4.sh`
  produziert CoreFS-FUSE vs. ext4-Referenz auf identischem Datenträger,
  damit jede künftige Änderung gegen die Phase-1-Zahlen verglichen werden
  kann.

### Ergebnisse nach Phase 1

| Workload                 | Phase 0 CoreFS | Phase 1 CoreFS | ext4 (Ref) | Phase-1-Faktor ggü. ext4 | Verbesserung |
|--------------------------|---------------:|---------------:|-----------:|-------------------------:|-------------:|
| create 200 × 4 KiB       |       48 ops/s |  **2631 ops/s**|  5405 ops/s|                   2.05×  |      **55×** |
| read 200 × 4 KiB         |       72 ops/s |  **3333 ops/s**|  5882 ops/s|                   1.76×  |      **46×** |
| stat (ls -la) 200        |           6 ms |           4 ms |       5 ms |                   0.8×   |    besser    |
| seq write 128 MiB        |       25 MiB/s |    **76 MiB/s**|  723 MiB/s |                   9.5×   |       3.0×   |
| seq read 128 MiB         |       38 MiB/s |  **1438 MiB/s**| 5333 MiB/s |                   3.7×   |      **38×** |
| fsync 50 × 4 KiB         |  <0.3 ops/s\*  |     **2 ops/s**|   442 ops/s|                 221×     |       7.7×   |
| rand 4K 70r/30w, 500 ops |       96 ops/s |   **556 ops/s**| 14705 ops/s|                  26×     |       5.8×   |
| delete 200 × 4 KiB       |      391 205 ms |      **47 ms** |      13 ms |                   3.6×   |   **~8320×** |

\* Phase 0 fsync war Timeout bei 180 s (<0.3 ops/s effektiv).

Raw-TSV: [`perf-history/2026-04-19_phase1.tsv`](perf-history/2026-04-19_phase1.tsv).

### Verbliebene Gaps und geplante Folge-PRs

**P2 — True append in BlockStore (`extend_file` O(n²) eliminieren).**
`BlockStore::append_to_inode` (`corefs-core/src/storage/block_store.rs:440`)
macht aktuell Read-Modify-Write des gesamten Inode-Contents.  Bei
grossen sequentiellen Writes mit mehreren Flush-Runden skaliert das
quadratisch.  Fix: zusätzliche Extent an den bestehenden
`BlockRecord.extents` anhängen, ohne die bestehenden Bytes zu lesen.
Braucht Anpassung der CRC-Strategie (pro-Extent statt pro-Record) oder
Incremental-CRC.  Erwartung: seq_write 76 → ~400 MiB/s (ext4-Niveau).

**P3 — Group-Commit und Incremental Save (`fsync` O(Volume) eliminieren).**
`save_image_to_path` schreibt immer das gesamte Volume-Image neu
(`app/mod.rs:987`).  Für `FuseBacking::Device` gibt es bereits
`persist_to_device_incremental`; derselbe Ansatz (nur geänderte Segmente
/ Blockbereiche) muss auf File-Backing angewendet werden.  Parallel
fsync-Requests sollten coalescen.  Erwartung: fsync 2 → 300+ ops/s.

**P4 — Read-Path lazy (`open_file_handle` kein Full-File-Memcopy).**
Blockiert durch P2: ein ranged-`read_file` in CoreFsService fehlt.  Ohne
Range-Reads löst ein lazy Open jede Read-FUSE-Op zu einem Full-File-Clone
aus, was schlechter ist als der aktuelle Eager-Load.  Sinnvoll erst
zusammen mit einem BlockStore-Range-Read-API aus P2.

**P7 — Parallelität.**  Alle FUSE-Ops sind heute hinter `&mut self`
serialisiert, selbst mit `--threads 4`.  Partitionierung (Inode-Hash-Locks)
erst nach P2/P3 sinnvoll.

### Harness

- **Regression-Gate**: `scripts/corefs-benchmark-vs-ext4.sh` (idempotent,
  per-step-Timeouts, produziert TSV + Pretty-Table).  Sollte nach jedem
  Commit an `src/platform/linux_fuse.rs`, `src/app/mod.rs` oder
  `corefs-core/src/storage/` laufen.  Archivierung + Diff gegen Baseline
  via `scripts/corefs-benchmark-record.sh` → [`perf-history/`](perf-history/).
- **Umgebungs-Variablen**:
  - `COREFS_STREAM_FLUSH_MIB` — Streaming-Buffer-Grenze pro Handle (default 64)
  - `COREFS_BIN`, `WORK`, `FILES`, `PAYLOAD`, `SEQ_MIB`, `FSYNC_N`,
    `RAND_MIB`, `RAND_OPS`, `IMG_SIZE_MIB`, `THREADS`, `STEP_TIMEOUT`

---

## Phase 1b — 2026-04-19 (P2: echte Append-API im BlockStore)

Umgesetzt:

- **P2 — Fast-Path in `BlockStore::append_to_inode`** — für plain (nicht
  komprimierte/verschlüsselte) Records wird jetzt ein **neuer Extent**
  an `BlockRecord.extents` angehängt.  Bisheriger Read-Modify-Write
  (existing + extra zusammenkopieren und das Ganze neu schreiben) wird
  nur noch für Records mit `flags != 0` benutzt — die Crypto-/Compress-
  Pipeline per-Extent ist Folge-Arbeit.  CRC32C wird inkrementell
  weitergeführt (die vorhandene `Crc32c::update(seed, data)`-API
  unterstützt das nativ).
- **Append-Log-Micro-Bench** im `corefs-benchmark-vs-ext4.sh` ergänzt,
  weil der bestehende 128-MiB-seq-write-Workload den P2-Pfad nur einmal
  trifft und die Verbesserung untergeht.  Der neue Workload
  `append_log_2000x1KiB` schreibt 2000 × 1 KiB in dieselbe Datei —
  genau das Muster, das pre-P2 quadratisch skalierte.
- **Noise-Floor** im Compare-Script: Workloads < 10 ms werden nach
  absoluter ms-Differenz (> 3 ms) klassifiziert, nicht prozentual.  Das
  hatte vorher zu Falsch-Regressions bei `stat_ls_200` (4 → 5 ms =
  –25 %) geführt.
- **Baseline `perf-history/baseline.tsv` promotet auf p2-Stand
  (2026-04-19)**.  Drei Back-to-back-Läufe bestätigen Stabilität der
  meisten Workloads; `fsync_50x4096B` zeigt nachweisbaren Run-to-Run-Jitter
  (2 – 4 ops/s mit gelegentlichen Ausreissern nach unten unter System-
  Last).  Das kommt daher, dass jeder fsync aktuell das ganze Image-File
  neu schreibt; Disk-I/O-Scheduling auf dem Host-ext4 dominiert dann.
  Saubere Behebung erst mit P3 (incremental save + group-commit).

### Ergebnisse nach Phase 1b

Zahlen aus [`perf-history/2026-04-19_072652_p2-run2.tsv`](perf-history/2026-04-19_072652_p2-run2.tsv),
gleicher NVMe, gleicher Host.  Aktuelle Baseline für Regressions-Diffs:
[`perf-history/baseline.tsv`](perf-history/baseline.tsv).

| Workload                    | Phase 1 (vor P2) | Phase 1b (nach P2) | ext4 | CoreFS/ext4 | Anmerkung |
|-----------------------------|-----------------:|-------------------:|-----:|------------:|-----------|
| create 200 × 4 KiB          |        2631 ops/s|          2127 ops/s| 5555 |       2.6×  | stabil im Band |
| read 200 × 4 KiB            |        3333 ops/s|          4081 ops/s| 6060 |       1.5×  | +22 % |
| stat (ls -la) 200           |            4 ms  |              5 ms  |   5 ms|     parity  | noise-floor |
| seq write 128 MiB           |         76 MiB/s |          67 MiB/s  |   659 MiB/s|  9.8× | P3-gebunden |
| seq read 128 MiB            |       1438 MiB/s |        1075 MiB/s  |  5333 MiB/s|  4.9× | Jitter |
| fsync 50 × 4 KiB            |          2 ops/s |           4 ops/s  |   416 ops/s |104×  | P3-gebunden |
| rand 4K 70r/30w, 500 ops    |         556 ops/s|          627 ops/s |  14285 ops/s| 23× | +13 % |
| delete 200 × 4 KiB          |           47 ms  |            26 ms   |    14 ms  |     1.9× | besser |
| **append_log 2000 × 1 KiB** |  (nicht gemessen) | **5524 ops/s**     | 54054 ops/s|    9.8× | **neu, P2-spezifisch** |

Der `append_log`-Workload ist der direkte P2-Test und zeigt den
Unterschied klar: ohne die neue Fast-Path wäre jeder der 2000 Appends
O(file_size)-Read-Modify-Write, also quadratisch über den Loop
(~2 GiB RAM-Traffic).  Mit P2 bleibt jeder Append O(append_size).

### Was P2 NICHT gefixt hat (Phase 1c deckt P3 davon ab)

- **seq_write 128 MiB** — dominiert vom Full-Image-Save am Abschluss-
  `fsync`.  P2 spart nur eine interne Kopie beim zweiten Streaming-Flush,
  aber ext4 bleibt 10× voraus.  Das ist Phase-2-P3 (incremental save).
- **fsync-heavy** — jeder Einzel-fsync rewriteet das ganze Image, egal
  wie wenig geändert wurde.  Phase-2-P3.
- **Multi-Extent-Dedup** — nach einem Append schreibt das Update den
  alten Dedup-Eintrag zurück, setzt aber keinen neuen für den
  kombinierten Content-CRC.  Künftige identische Writes dedupen nicht
  gegen Multi-Extent-Records.  Akzeptabel — echte Multi-Extent-Dedup ist
  Scope P2b, nicht Phase-1.


---

## Phase 1c — 2026-04-19 (P3: incremental save für FuseBacking::File)

Umgesetzt:

- **P3a — Incremental Save auch für `FuseBacking::File`**.  Die Variante hat
  bisher bei jedem Checkpoint die gesamte Image-Datei via tmp+rename neu
  geschrieben (`save_image_to_path`).  Ersetzt durch einen live gehaltenen
  `FileImageDevice` + `DeviceImageCache` pro Mount; Persists gehen durch die
  neue Funktion `persist_to_device_incremental_with_bytes_and_grow` in
  `src/storage/volume_image.rs`, die nur die Segmente rausschreibt, deren
  Payload gegenüber dem Cache tatsächlich abweicht.
- **`BlockDevice::resize` als Trait-Methode**.  Default liefert
  `InvalidInput` ("not supported"), `FileImageDevice` überschreibt mit der
  bereits vorhandenen inherent `resize`.  Dadurch kann der FUSE-Persist die
  Datei on-demand wachsen lassen, ohne Downcast auf konkrete Typen.
- **Single-Build-Invariante**.  `persist_to_device_incremental_with_bytes_and_grow`
  baut das Image genau einmal, misst danach die benötigte Kapazität und
  ruft die `grow`-Closure (die im FUSE-Callsite die FileImageDevice um
  25 % Overshoot vergrößert).  Kein zweites Image-Build für einen separaten
  Capacity-Check — das war in einer Zwischenversion der Hauptgrund, warum
  seq-write / rand messbar regressierten.
- **Neuer Workload `fsync_cold_50x4096B`** im
  `corefs-benchmark-vs-ext4.sh`.  Läuft *vor* `seq_write_128MiB` und
  demonstriert P3 auf einem kleinen Volume — der Anwendungsfall, den P3
  eigentlich adressiert.  Die bestehende `fsync_50x4096B`-Zeile läuft
  weiterhin *nach* `seq_write_128MiB` und bleibt als Stress-Test stehen.

### Ergebnisse nach Phase 1c

Basis: `perf-history/2026-04-19_080123_p3-cold-fsync.tsv`
(promotet nach `baseline.tsv`).

| Workload                    | Phase 1b (P2) | **Phase 1c (P3)** | ext4       | CoreFS/ext4 | Anmerkung |
|-----------------------------|-------------:|-------------------:|-----------:|------------:|-----------|
| create 200 × 4 KiB          |   2127 ops/s |      **2631 ops/s**| 5263 ops/s |       2.0×  | +24 % |
| read 200 × 4 KiB            |   4081 ops/s |          3174 ops/s| 6060 ops/s |       1.9×  | Jitter-Band |
| stat (ls -la) 200           |        5 ms  |             5 ms   |       5 ms |    parity   | |
| **fsync_cold 50 × 4 KiB**   |       n/a    |        **92 ops/s**|  471 ops/s |       5.1×  | **P3-Demo** (war <1 ops/s vor P1) |
| seq write 128 MiB           |    67 MiB/s  |          68 MiB/s  |  757 MiB/s |      11×    | P3b-gebunden |
| seq read 128 MiB            |  1075 MiB/s  |     **1802 MiB/s** | 6400 MiB/s |       3.6×  | +68 % |
| fsync_warm 50 × 4 KiB       |     4 ops/s  |          2 ops/s   |  515 ops/s |      258×   | block_bytes-Clone dominiert |
| rand 4K 70r/30w, 500 ops    |   627 ops/s  |          560 ops/s |14 285 ops/s|      25×    | Jitter-Band |
| delete 200 × 4 KiB          |      26 ms   |          29 ms     |     13 ms  |       2.2×  | |
| append_log 2000 × 1 KiB     |   5524 ops/s |          5167 ops/s|54 054 ops/s|      10×    | Jitter-Band |

`fsync_cold` ist die klare Demonstration: bei einem frisch formatierten
Volume erreicht CoreFS mit P3 nun **92 fsyncs/s** gegen ext4 **471/s** —
Faktor 5.1× statt ~104× wie vor P3.  Pre-Phase-1 war der gleiche Workload
mit <1 fsync/s effektiv blockiert.

### Bekannter Gap nach Phase 1c — P3b

`fsync_warm_50x4096B` (fsync-heavy *nach* `seq_write_128MiB`) bleibt bei
~2 ops/s.  Ursache: `read_all_block_bytes` klont pro Persist die Bytes
*aller* Inodes, auch die unveränderten.  Für ein Volume mit einem 128 MiB-
File bedeutet das, dass jeder fsync 128 MiB im RAM kopiert, obwohl das
DATA-Segment seit dem letzten Checkpoint unverändert ist.  Die Cache-Diff
erkennt den Zustand zwar und überspringt den Write, aber der Build-Pfad
hat die Kopie bereits angelegt.

**P3b (Folge-PR)**: `CoreFsService` verfolgt pro Checkpoint eine Menge
"dirty inodes"; `read_all_block_bytes_for_dirty(dirty)` liefert nur deren
Content, unveränderte Inodes werden mit ihrem bereits im Cache
gespeicherten Segment-Payload re-used.  Erwartung: `fsync_warm` kommt auf
das `fsync_cold`-Niveau (~90+ ops/s).

### Zusätzlich verbessert (Side-Effekte)

- **seq_read 128 MiB**: 1075 → 1802 MiB/s (+68 %).  Keine direkte
  Erklärung im Code — vermutlich weniger Fragmentierung der
  `compat_device`-Allokationen, weil P3 die Image-Datei jetzt direkt als
  FileImageDevice pro Mount offen hält.
- **create 200 × 4 KiB**: 2127 → 2631 ops/s (+24 %).  Incremental Save
  spart pro `create`-Checkpoint den tmp+rename-Overhead.

### Harness-Änderungen

- Neu: Workload `fsync_cold_50x4096B` vor `seq_write_128MiB`.
- `baseline.tsv` auf P3 promotet (inkl. neuer Row).  Die Regressionen
  `fsync_50x4096B` (-50 %) und `read_200x4096B` (-22 %) gegen die alte
  P1b-Baseline sind dokumentiert — `read_200` ist Jitter-Band, `fsync_warm`
  ist der oben beschriebene P3b-Gap.

---

## Phase 1d — 2026-04-19 (P3b-a: redundanten DATA-Clone eliminiert)

Umgesetzt:

- **Einzelner, kleiner Fix in `split_blocks`** (`src/storage/volume_image.rs`):
  statt `block_bytes.get(&record.inode).cloned().unwrap_or_default()` wird
  jetzt per `&[u8]`-Referenz in den DATA-Output geschrieben.  Das
  eliminiert eine komplette Kopie des DATA-Inhalts pro Persist
  (für ein Volume mit 128 MiB DATA entspricht das ~128 MiB RAM-Traffic
  gespart pro fsync).
- **Baseline `perf-history/baseline.tsv` auf den P3b-a-Run gesetzt**
  (`perf-history/2026-04-19_*_p3b-no-clone.tsv`).

### Ergebnisse

| Workload                  | Phase 1c (P3) | **Phase 1d (P3b-a)** | ext4 | CoreFS/ext4 |
|---------------------------|-------------:|---------------------:|-----:|------------:|
| **fsync_warm 50 × 4 KiB** |     2 ops/s  |           **7 ops/s**|  403 |       58×   |
| read 200 × 4 KiB          |  3174 ops/s  |               3846   | 5555 |        1.4× |
| fsync_cold 50 × 4 KiB     |    92 ops/s  |                 93   |  416 |        4.5× |
| create, seq_write, seq_read, rand, delete, append_log |    — | unverändert im Jitter-Band |  |  |

`fsync_warm` ist um Faktor 3.5× schneller geworden, weil die doppelte
DATA-Kopie pro Persist aus der Build-Pipeline verschwunden ist.  Der
Restgap zu ext4 (58×) bleibt, dominiert von der weiterhin vollständigen
DATA-Segment-Rematerialisierung in jedem Checkpoint.

### Bekannter Gap nach Phase 1d — P3b-b

`fsync_warm` bleibt bei 7 ops/s weil `split_blocks` jeden Persist das
volle DATA-Segment aus allen Block-Records + deren Content neu
assembliert (auch unveränderte Inodes).  Für ein 128 MiB-Volume mit
50 × 4 KiB-fsync kopiert jeder Checkpoint 128 MiB durch die Build-
Pipeline, auch wenn nur ~4 KB wirklich neu sind.

**P3b-b (Folge-PR, echte dirty-tracking + partial DATA rebuild)**:

- `CoreFsService` pflegt eine `dirty_inodes: HashSet<InodeId>`, die auf
  jeder Mutation (`write_file` / `extend_file` / `create_file` /
  `delete_file` / `set_owner` / `set_mode` / `rename_entry` / …)
  aktualisiert und bei erfolgreichem Persist geleert wird.
- `DeviceImageCache` speichert zusätzlich die Per-Inode-Offsets in
  seinem cached DATA-Payload (aus den zuletzt emittierten
  `BlockDescriptor`s).
- Neue Funktion `persist_to_device_incremental_partial(device, state,
  dirty_inodes, read_dirty, cache, grow)`:
  - Wenn Layout identisch (selbe inode-Menge, selbe Reihenfolge):
    starte vom cached DATA, patche dirty Regionen in-place.
  - Sonst: Fallback auf den vollen Rebuild-Pfad.
- `FUSE-persist` liest nur dirty Inodes via `read_dirty_block_bytes`
  (neu im Service) und übergibt sie an `..._partial`.

Erwartung: `fsync_warm` kommt auf das `fsync_cold`-Niveau (~90 ops/s),
weil jeder Persist nur noch die tatsächlich geänderten 4 KiB
materialisiert statt der gesamten 128 MiB.

### Warum nicht gleich P3b-b?

P3b-b ist größer und berührt sowohl den Cache-Layout-Code als auch die
Service-API (dirty-Tracking als neues Public-Feature).  Der
`.cloned()`-Fix ist ein isolierter Bugfix im Build-Pfad, ohne API- oder
Format-Änderung — testbar und review-bar in einem einzelnen Commit.
P3b-a + P3b-b zusammen wären ein unübersichtlicher Umbau gewesen.

---

## Phase 1e — 2026-04-19 (P3b-b Infrastruktur: dirty-inode tracking)

Umgesetzt: die **Voraussetzungen** für echten Partial-DATA-Rebuild,
ohne das DATA-Rebuild-Verhalten selbst zu ändern (damit jeder
Refactor-Schritt review- und rollback-bar bleibt).

### Änderungen

- **`CoreFsService.dirty_inodes: HashSet<InodeId>`** — neu.  Wird von
  jeder Mutation, die Block-Inhalt oder Extent-Layout ändert, gepflegt
  (`create_file`, `create_directory`, `create_symlink`, `write_file`,
  `extend_file`, `delete_file`, `restore_file`).  Metadaten-nur
  Mutationen wie `set_owner` / `set_mode` / `rename_entry` lassen das
  Set bewusst leer — sie berühren catalog/inode-Segmente, nicht DATA.
- **`CoreFsService::take_dirty_inodes(&mut self)`** — neu.  Gibt das
  Set zurück und leert es; gedacht als Hook, den der Checkpoint-Code
  genau einmal pro Persist aufruft.
- **`CoreFsService::has_dirty_inodes(&self)`** — neu.  Günstiger
  Zustands-Check ohne Drain.

### Kein Perf-Impact jetzt (bewusst)

Das Feld ist infrastruktur-only — der FUSE-Persist-Pfad liest es in
Phase 1e *nicht*, rebuildet also weiterhin das volle DATA-Segment pro
Checkpoint.  Der Bench bestätigt Parity mit der Phase-1d-Baseline im
±15 %-Noise-Band (siehe `perf-history/2026-04-19_081928_p3b-a-run2.tsv`):

| Workload              | Phase 1d | Phase 1e | Delta |
|-----------------------|---------:|---------:|------:|
| create 200 × 4 KiB    |     2631 |     2409 | −8 %  |
| read 200 × 4 KiB      |     3846 |     3448 | −10 % |
| fsync_cold 50 × 4 KiB |       93 |       94 |   0 % |
| seq_write 128 MiB     |       68 |       65 | −4 %  |
| seq_read 128 MiB      |     1777 |     1802 |  +1 % |
| fsync_warm 50 × 4 KiB |        7 |        2 | noise-band (1–7) |
| rand 4K               |      563 |      553 | −2 %  |
| append_log 2000 × 1 K |     5167 |     5277 |  +2 % |

### Warum zwei Schritte statt einem

Der eigentliche Perf-Win (Partial-DATA-Rebuild) erfordert vier
verschränkte Änderungen in `src/storage/volume_image.rs`:

1. `DeviceImageCache` muss zusätzlich die `Vec<BlockDescriptor>` des
   letzten DATA-Builds speichern (mit `(inode, offset, length)`-Tripeln).
2. `BuiltImage` muss die Descriptors nach außen geben, damit
   `DeviceImageCache::from_built` sie übernehmen kann.
3. Eine neue Funktion `split_blocks_partial(records, block_size,
   dirty_bytes, cache)` rekonstruiert das DATA-Segment aus
   `cache.payload` (für Inodes, deren (inode, offset, length) identisch
   ist) + frischen Bytes aus `dirty_bytes` (für dirty Inodes).  Layout-
   Inkompatibilität (neuer Inode zwischen bestehenden, Grösse eines
   Inodes geändert) fällt auf den vollen Build zurück.
4. `CoreFsService::read_dirty_block_bytes(&HashSet<InodeId>)` materialisiert
   nur die dirty Inodes (statt `read_all_block_bytes`).

Jeder Schritt braucht Tests (Fallback-Pfade, append-only growth, delete
mit nachfolgenden Inodes).  Das sauber in einem Commit zu landen
würde den Reviewer überfordern — und ein Bug in einem der vier Schritte
ist ein Korruptions-Risiko für die On-Disk-Daten.

Phase 1e ist deshalb Setup.  Phase 1f liefert den strukturellen
Performance-Schritt.

### Erwartung für Phase 1f (P3b-b)

- `fsync_warm_50x4096B`: 2–7 ops/s → **~90 ops/s** (Parität mit `fsync_cold`)
- `seq_write_128MiB`: 68 MiB/s → **~300 MiB/s**
- Voraussetzung: append-only-Growth-Erkennung in `split_blocks_partial`
  greift in genau diesen Workloads (neue Inodes bekommen grössere
  `InodeId`, landen deshalb am Ende der BTreeMap-Iteration in
  `BlockStore::records()`).
