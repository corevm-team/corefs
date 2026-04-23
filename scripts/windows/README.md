# CoreFS Windows Scripts

Diese Wrapper benutzen die normale `corefs`-CLI.

Aufloesungsreihenfolge fuer normale Kurzbefehle:

- `dist\bin\corefs.exe`
- `target\release\corefs.exe`
- `cargo run --release --features windows-winfsp --bin corefs -- ...`

Hintergrund-Mounts und gemountete Benchmarks starten bevorzugt eine echte `corefs.exe`
aus `dist\bin` oder `target\release`, damit die gespeicherte PID direkt zum
WinFSP-Hostprozess gehoert.

Verfuegbare Skripte:

- `corefs-benchmark.ps1` / `.bat`
- `corefs-benchmark-mounted.ps1` / `.bat`
- `corefs-benchmark-vs-ntfs.ps1` / `.bat`
- `corefs-mkfs-image.ps1` / `.bat`
- `corefs-mount-image.ps1` / `.bat`
- `corefs-unmount-image.ps1` / `.bat` zeigt den korrekten Foreground-Unmount-Hinweis
- `install-winfsp.ps1` / `.bat`
- `install-corefs-windows.ps1` / `.bat`

Installation:

```powershell
# Nur WinFSP Runtime installieren. Standard: winget, sonst GitHub-Release-MSI.
.\scripts\windows\install-winfsp.ps1

# CoreFS fuer native Windows-Mounts bauen und nach dist\bin kopieren.
.\scripts\windows\install-corefs-windows.ps1

# Alles in einem Schritt: WinFSP installieren, CoreFS bauen, dist\bin optional in User-PATH aufnehmen.
.\scripts\windows\install-corefs-windows.ps1 -InstallWinFsp -AddToUserPath
```

Beispiele:

```powershell
.\scripts\windows\corefs-mkfs-image.ps1 -ImagePath .\demo.img -Demo
.\scripts\windows\corefs-mkfs-image.ps1 -Path .\images\demo.img -Demo
.\scripts\windows\corefs-mkfs-image.ps1 -ImagePath .\fast.img -Profile performance
.\scripts\windows\corefs-mount-image.ps1 -ImagePath .\demo.img -DriveLetter X: -ReadWrite
.\scripts\windows\corefs-mount-image.ps1 -Path .\images\demo.img -DriveLetter X: -ReadWrite
.\scripts\windows\corefs-mount-image.ps1 -ImagePath .\demo.img -DriveLetter X: -ReadWrite -Background
.\scripts\windows\corefs-unmount-image.ps1 -DriveLetter X:
.\scripts\windows\corefs-benchmark.ps1 -LogPath .\PERFORMANCE_LOG.windows.md
.\scripts\windows\corefs-benchmark-mounted.ps1 -ImagePath .\bench.img -DriveLetter X: -LogPath .\PERFORMANCE_LOG.windows-mount.md
.\scripts\windows\corefs-benchmark-vs-ntfs.ps1 -ImagePath .\target\windows-bench\corefs.img -DriveLetter X:
```

Performance-History:

- `corefs-benchmark-mounted.ps1` und `corefs-benchmark-vs-ntfs.ps1` schreiben zusaetzlich timestamped TSV-Artefakte nach `perf-history`.
- Default-Dateien: `YYYY-MM-DD_HHMMSS_windows-mount.tsv` und `YYYY-MM-DD_HHMMSS_windows-vs-ntfs.tsv`.
- Mit `-HistoryLabel <label>` kann der Suffix angepasst werden.
- Mit `-NoPerfHistory` laesst sich das Archivieren fuer einen lokalen Wegwerf-Lauf abschalten.

Batch:

```bat
scripts\windows\corefs-mkfs-image.bat .\demo.img --demo
scripts\windows\corefs-mkfs-image.bat -ImagePath .\fast.img -Profile performance
scripts\windows\corefs-mount-image.bat .\demo.img X:
scripts\windows\corefs-mount-image.bat -ImagePath .\demo.img -DriveLetter X: -ReadWrite -Background
scripts\windows\corefs-unmount-image.bat -DriveLetter X:
scripts\windows\corefs-benchmark.bat
scripts\windows\corefs-benchmark-mounted.bat -ImagePath .\bench.img -DriveLetter X:
scripts\windows\corefs-benchmark-vs-ntfs.bat -ImagePath .\target\windows-bench\corefs.img -DriveLetter X:
```

Hinweis:

- Native Windows-Mounts laufen ueber WinFSP, nicht ueber `subst`.
- Relative `-ImagePath`/`-Path`, `-PidPath` und `-LogDir`-Angaben werden relativ zu deinem aktuellen Arbeitsordner aufgeloest, nicht relativ zum Repo oder zur `corefs.exe`.
- `-Profile performance` ist kein Windows-Sonderpfad: es erzeugt ein plattformneutrales CoreFS-Image ohne Versioning, Compression und Encryption fuer faire Rohdurchsatz-Messungen gegen NTFS/ext4.
- Build: `cargo build --features windows-winfsp`
- Voraussetzung: installierte WinFSP-2.x-Laufzeit. Die Rust-Bindings sind vendored; LLVM/libclang wird dafuer nicht lokal benoetigt.
- `install-winfsp.ps1` bevorzugt `winget install --id WinFsp.WinFsp` und kann alternativ das aktuelle MSI aus `winfsp/winfsp` GitHub Releases herunterladen.
- Ohne `-Background` bleibt der Mount-Prozess im Vordergrund. Zum Aushaengen Ctrl+C im Mount-Fenster druecken; CoreFS stoppt dann den WinFSP-Dispatcher und entfernt den Mountpoint.
- Mit `-Background` bleibt das Laufwerk nach Ende des Skripts aktiv. Die PID-Datei liegt standardmaessig unter `target\windows-mounts\corefs-X.mount.json`; unmounten mit `corefs-unmount-image.ps1 -DriveLetter X:`.
- `corefs-benchmark.ps1` nutzt die plattformneutralen CoreFS-Benchmark-Profile.
- `corefs-benchmark-mounted.ps1` erzeugt standardmaessig ein `performance`-Profil-Image, mountet es read-write ueber WinFSP und misst Create/Read/Sequential-Write/Sequential-Read/Delete direkt auf dem Windows-Laufwerk.
- `corefs-benchmark-vs-ntfs.ps1` ist das Windows-Pendant zu `corefs-benchmark-vs-ext4.sh`: CoreFS/WinFSP gegen ein direktes NTFS-Verzeichnis, inklusive Ops/s und MiB/s. Standardprofil ist `performance`; mit `-Profile default` misst du das volle Enterprise-Profil.
