# Beispiele

Alle Beispiele gehen von einem gebauten Binary aus:

```bash
cargo build --release
alias corefs=./target/release/corefs
```

## 1. Einstiegs-Workflow (in-memory)

```bash
corefs mkfs
corefs write /hello.txt "Hallo Welt"
corefs ls
corefs read /hello.txt
corefs snapshot initial
corefs status
```

## 2. Image-Lebenszyklus

```bash
corefs mkfs-image ./demo.img --demo
corefs load-image ./demo.img
corefs fsck-image ./demo.img
corefs optimize-image ./demo.img
```

## 3. FUSE-Read-only-Mount (Linux)

```bash
mkdir -p /tmp/corefs-ro
corefs mount-image ./demo.img /tmp/corefs-ro
ls /tmp/corefs-ro
cat /tmp/corefs-ro/readme.txt
fusermount -u /tmp/corefs-ro
```

## 4. FUSE-Read-write-Mount mit Time-Travel

```bash
mkdir -p /tmp/corefs-rw
corefs mount-image-rw ./demo.img /tmp/corefs-rw

# normale Schreibzugriffe
echo "v1" > /tmp/corefs-rw/note.txt
echo "v2" > /tmp/corefs-rw/note.txt

# Snapshot über CLI in separatem Prozess nicht möglich während gemountet.
# Daher vor dem Mount Snapshots anlegen oder nach Unmount.

# Time-Travel-Adressierung
cat /tmp/corefs-rw/note.txt@v1

# Snapshot-Browsing
ls /tmp/corefs-rw/.snapshots/

fusermount -u /tmp/corefs-rw
```

## 5. Blockgerät — kompletter sicherer Ablauf

```bash
# 1. Prüfen
sudo corefs probe-device /dev/sdb1

# 2. Optional: Fake-Stick-Vollscan (destruktiv!)
sudo corefs verify-device /dev/sdb1 --destructive --chunks 128

# 3. Formatieren
sudo corefs mkfs-device /dev/sdb1

# 4. Prüfen
sudo corefs fsck-device /dev/sdb1

# 5. Mounten
sudo mkdir -p /mnt/usb
sudo corefs mount-device-rw /dev/sdb1 /mnt/usb

# 6. Nutzen
echo "Hallo Stick" | sudo tee /mnt/usb/greeting.txt

# 7. Unmount
sudo fusermount -u /mnt/usb
```

## 6. Verschlüsseltes Volume

Die Default-Policy aktiviert Verschlüsselung. Inhalte werden transparent geschrieben und gelesen:

```bash
corefs mkfs
corefs write /secret.txt "sensitive"
corefs save-image ./secret.img
# secret.img enthält AEAD-verschlüsselte Payload; Lesen nur via CoreFS
corefs load-image ./secret.img
corefs read /secret.txt
```

## 7. Snapshot, Delete, Restore

```bash
corefs mkfs
corefs write /a.txt "alpha"
corefs snapshot initial
corefs delete /a.txt
corefs ls                    # a.txt fehlt
corefs restore /a.txt
corefs read /a.txt           # "alpha" zurück

# Secure-Delete blockiert Restore
corefs delete /a.txt --secure
corefs restore /a.txt        # Fehler: NotFound
```

## 8. Defragmentierung & Optimierung

```bash
corefs status                # zeigt fragmentation_percent
corefs defrag
corefs optimize              # Defrag + Heat-Reallocation
corefs save-image ./demo.img
corefs defrag-image ./demo.img
corefs optimize-image ./demo.img
```

## 9. Benchmarking mit Logging

```bash
corefs benchmark --profile balanced
corefs benchmark --profile snapshot-heavy --files 50 --snapshots 10
corefs benchmark-log ./PERFORMANCE_LOG.md --profile persist-heavy
```

## 10. End-to-End-Skript

```bash
./scripts/corefs-e2e-linux-rw.sh
```

Dieses Skript führt `mkfs-image`, RW-Mount, Shell-Operationen, optional einen ZIP-Workload, Unmount und eine Revalidierung aus.

## 11. Tests ausführen

```bash
cargo test                               # alle Unit-Tests
./scripts/testing/install-test-suites.sh # einmalig
./scripts/testing/run-all.sh --quick
./scripts/testing/run-stress.sh --workers 8 --duration 120
```

## 12. Diagnose

```bash
cargo run -- diagnose-mount ./demo.img /tmp/corefs-mnt --create
./scripts/corefs-doctor.sh
./scripts/corefs-trace-mount.sh ./demo.img /tmp/corefs-mnt
```
