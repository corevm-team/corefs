# CoreFS — Intensiv-Testinfrastruktur

Dieses Verzeichnis enthält Skripte zum intensiven Testen von CoreFS unter Linux mittels etablierter Dateisystem-Testsuiten.

## Übersicht

| Skript | Zweck |
|---|---|
| `install-test-suites.sh` | Installiert pjdfstest und xfstests lokal |
| `run-pjdfstest.sh` | POSIX-Compliance-Tests (Berechtigungen, Symlinks, Verzeichnisse, ...) |
| `run-xfstests.sh` | Linux-Kernel-Dateisystem-Testsuite (generische Tests) |
| `run-stress.sh` | Paralleler Stresstest (Dateien, Verzeichnisse, Renames, Symlinks) |
| `run-all.sh` | Alle drei Suiten nacheinander ausfuehren |

## Voraussetzungen

- **Linux** mit FUSE3-Unterstützung
- **Rust-Toolchain** (cargo) oder vorgebautes CoreFS-Binary unter `dist/bin/corefs`
- Build-Tools: `gcc`, `make`, `automake`, `autoconf`, `git`, `pkg-config`
- Perl TAP-Harness (`prove`) für pjdfstest-Auswertung
- Root-Rechte für xfstests (einige Tests benötigen mount/umount)

### Debian/Ubuntu

```bash
sudo apt install build-essential git automake autoconf pkg-config \
  fuse3 libfuse3-dev libacl1-dev attr libaio-dev uuid-dev \
  xfsprogs e2fsprogs btrfs-progs dump xfslibs-dev perl
```

### Fedora/RHEL

```bash
sudo dnf install gcc make git automake autoconf pkgconfig \
  fuse3 fuse3-devel libacl-devel attr libaio-devel uuid-devel \
  xfsprogs e2fsprogs btrfs-progs dump xfsprogs-devel perl-Test-Harness
```

## Schnellstart

```bash
# 1. Testsuiten installieren (einmalig)
./scripts/testing/install-test-suites.sh

# 2. Alle Tests ausfuehren
./scripts/testing/run-all.sh

# 3. Oder einzeln:
./scripts/testing/run-pjdfstest.sh
./scripts/testing/run-xfstests.sh
./scripts/testing/run-stress.sh
```

## Detaillierte Anleitung

### pjdfstest — POSIX-Compliance

[pjdfstest](https://github.com/pjd/pjdfstest) prüft die Einhaltung des POSIX-Standards für Dateisystem-Operationen.

**Testgruppen:**
- `chmod`, `chown` — Berechtigungen
- `link`, `symlink` — Hard- und Symlinks
- `mkdir`, `rmdir` — Verzeichnisoperationen
- `open`, `rename`, `unlink` — Dateizugriff
- `truncate`, `ftruncate` — Dateigrössenänderung
- `utimensat` — Zeitstempel
- `misc` — Verschiedenes

```bash
# Alle Tests
./scripts/testing/run-pjdfstest.sh

# Nur bestimmte Gruppen
./scripts/testing/run-pjdfstest.sh --groups chmod,mkdir,symlink

# Eigenes Image verwenden
./scripts/testing/run-pjdfstest.sh --image /pfad/zum/image.img --mount /pfad/zum/mount
```

**Erwartete Ergebnisse:** FUSE-Dateisysteme bestehen typischerweise nicht alle pjdfstest-Tests, da einige POSIX-Semantiken (z.B. `chown` ohne Root, bestimmte `link`-Verhalten) von FUSE nicht vollständig unterstützt werden. Die Ergebnisse zeigen, wo CoreFS steht und wo Nachbesserung sinnvoll ist.

### xfstests — Kernel-Dateisystem-Tests

[xfstests](https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git) ist die Standard-Testsuite des Linux-Kernels für Dateisysteme. Sie benötigt zwei Partitionen/Images:

- **TEST_DEV / TEST_DIR** — Haupttest-Dateisystem (bleibt gemountet)
- **SCRATCH_DEV / SCRATCH_MNT** — Wird zwischen Tests neu formatiert

```bash
# Standard (generic/quick)
./scripts/testing/run-xfstests.sh

# Bestimmte Gruppen
./scripts/testing/run-xfstests.sh --groups generic/posix,generic/perms

# Einzelne Tests
./scripts/testing/run-xfstests.sh --tests generic/001,generic/002

# Tests ausschliessen
./scripts/testing/run-xfstests.sh --exclude generic/050,generic/051

# Mit Root-Rechten (empfohlen)
sudo ./scripts/testing/run-xfstests.sh
```

**Wichtig:** xfstests wurde für Kernel-Dateisysteme entwickelt. Bei FUSE-Dateisystemen sind viele Fehler erwartet. Relevante Testgruppen für CoreFS:

| Gruppe | Beschreibung |
|---|---|
| `generic/quick` | Schnelle generische Tests |
| `generic/posix` | POSIX-Konformität |
| `generic/perms` | Berechtigungsmodell |
| `generic/attr` | Erweiterte Attribute |
| `generic/rw` | Lese-/Schreibtests |
| `generic/auto` | Automatisierbare Tests |

### Stresstest — Parallelität und Stabilität

Der Stresstest führt gleichzeitig verschiedene Dateisystem-Operationen aus:

- **File-Ops:** Dateien erzeugen, lesen, überschreiben, löschen
- **Dir-Ops:** Tiefe Verzeichnisstrukturen auf-/abbauen
- **Link/Rename:** Symlinks, Umbenennung
- **Concurrent Append:** Parallele Schreibzugriffe auf dieselbe Datei

```bash
# Standard (4 Worker, 120 Sekunden)
./scripts/testing/run-stress.sh

# Intensiv
./scripts/testing/run-stress.sh --workers 8 --duration 300 --file-count 500

# Schnell
./scripts/testing/run-stress.sh --workers 2 --duration 30 --file-count 50
```

Nach dem Stresstest wird `fsck-image` ausgeführt, um die Dateisystem-Integrität zu verifizieren.

### Gesamtlauf

```bash
# Alles ausfuehren
./scripts/testing/run-all.sh

# Schnelldurchlauf
./scripts/testing/run-all.sh --quick

# Ohne xfstests (z.B. ohne Root)
./scripts/testing/run-all.sh --skip-xfstests

# Images behalten fuer Analyse
./scripts/testing/run-all.sh --keep
```

## Verzeichnisstruktur

```
scripts/testing/
├── TESTING.md              ← Diese Dokumentation
├── install-test-suites.sh  ← Installer
├── run-pjdfstest.sh        ← POSIX-Tests
├── run-xfstests.sh         ← xfstests
├── run-stress.sh           ← Stresstest
├── run-all.sh              ← Gesamtlauf
├── suites/                 ← Geklonte Testsuiten (gitignored)
│   ├── pjdfstest/
│   └── xfstests/
└── results/                ← Testergebnisse (gitignored)
    ├── pjdfstest-YYYYMMDD-HHMMSS.log
    ├── xfstests-YYYYMMDD-HHMMSS.log
    ├── stress-YYYYMMDD-HHMMSS.log
    └── summary-YYYYMMDD-HHMMSS.log
```

## Ergebnisse interpretieren

### Priorisierung

1. **Stresstest-Fehler** → Stabilitätsproblem, hohe Priorität
2. **pjdfstest-Fehler in `open`, `mkdir`, `rename`, `unlink`** → Kernfunktionalität
3. **pjdfstest-Fehler in `chmod`, `chown`** → Berechtigungsmodell, oft FUSE-bedingt
4. **xfstests-Fehler** → Einzeln bewerten; viele sind bei FUSE erwartet

### Bekannte Einschränkungen bei FUSE

- `chown`/`chmod` ohne Root oft eingeschränkt
- Hardlinks können limitiert sein
- `mknod`, `mkfifo` je nach FUSE-Konfiguration
- Einige Locking-Semantiken weichen ab
- `fallocate` typischerweise nicht unterstützt
