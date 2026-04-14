# CoreFS Windows Scripts

Diese Wrapper benutzen die normale `corefs`-CLI.

Aufloesungsreihenfolge:

- `cargo run --release -- ...`
- `dist\bin\corefs.exe`
- `target\release\corefs.exe`

Verfuegbare Skripte:

- `corefs-mkfs-image.ps1` / `.bat`
- `corefs-mount-image.ps1` / `.bat`
- `corefs-unmount-image.ps1` / `.bat`

Beispiele:

```powershell
.\scripts\windows\corefs-mkfs-image.ps1 -ImagePath .\demo.img -Demo
.\scripts\windows\corefs-mount-image.ps1 -ImagePath .\demo.img -DriveLetter X: -ReadWrite
.\scripts\windows\corefs-unmount-image.ps1 -DriveLetter X:
```

Batch:

```bat
scripts\windows\corefs-mkfs-image.bat .\demo.img --demo
scripts\windows\corefs-mount-image.bat .\demo.img X: --staging C:\Temp\CoreFS-X
scripts\windows\corefs-unmount-image.bat X:
```

Hinweis:

- Die eigentliche Logik liegt in der `corefs`-CLI und der Windows-Integration unter `src/platform/windows/`.
- Die Wrapper duplizieren keine Dateisystemlogik, sondern rufen nur die CLI auf.
