param(
    [string]$InstallDir = ".\dist\bin",
    [switch]$InstallWinFsp,
    [switch]$SkipBuild,
    [switch]$AddToUserPath,
    [switch]$Force
)

. (Join-Path $PSScriptRoot "corefs-common.ps1")

$repoRoot = Get-CoreFsRepoRoot
$resolvedInstallDir = [System.IO.Path]::GetFullPath((Join-Path $repoRoot $InstallDir))

if ($InstallWinFsp) {
    & (Join-Path $PSScriptRoot "install-winfsp.ps1")
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and -not $SkipBuild) {
    throw "cargo wurde nicht gefunden. Installiere Rust via rustup und die MSVC-Buildtools, oder nutze -SkipBuild mit vorhandenem target\release\corefs.exe."
}

if (-not $SkipBuild) {
    Push-Location $repoRoot
    try {
        & cargo build --release --features windows-winfsp --bin corefs
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
    }
    finally {
        Pop-Location
    }
}

$sourceExe = Join-Path $repoRoot "target\release\corefs.exe"
if (-not (Test-Path $sourceExe)) {
    throw "corefs.exe wurde nicht gefunden: $sourceExe"
}

if ((Test-Path $resolvedInstallDir) -and -not $Force) {
    Write-Host "Installationsverzeichnis existiert: $resolvedInstallDir"
}
New-Item -ItemType Directory -Force -Path $resolvedInstallDir | Out-Null

$targetExe = Join-Path $resolvedInstallDir "corefs.exe"
Copy-Item -LiteralPath $sourceExe -Destination $targetExe -Force

if ($AddToUserPath) {
    $currentPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $parts = @()
    if ($currentPath) {
        $parts = $currentPath -split ";"
    }
    if ($parts -notcontains $resolvedInstallDir) {
        $newPath = if ($currentPath) { "$currentPath;$resolvedInstallDir" } else { $resolvedInstallDir }
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        Write-Host "User PATH erweitert. Neue Terminals sehen corefs.exe direkt."
    }
}

Write-Host "CoreFS Windows binary installed: $targetExe"
if (-not (Test-CoreFsWinFspInstalled)) {
    Write-Host "Hinweis: WinFSP Runtime wurde nicht erkannt. Mounts funktionieren erst nach WinFSP-Installation."
}
