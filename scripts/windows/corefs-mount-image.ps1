param(
    [string]$ImagePath = ".\corefs-volume.img",
    [string]$DriveLetter = "X:",
    [switch]$ReadWrite,
    [switch]$Background,
    [string]$PidPath = "",
    [string]$LogDir = ".\target\windows-mounts",
    [int]$ReadyTimeoutSeconds = 30
)

. (Join-Path $PSScriptRoot "corefs-common.ps1")

Assert-CoreFsWinFspInstalled

$command = if ($ReadWrite) { "mount-image-rw" } else { "mount-image" }
$driveRoot = Normalize-CoreFsDriveRoot -Value $DriveLetter
$drivePath = "$driveRoot\"
$resolvedImage = [System.IO.Path]::GetFullPath($ImagePath)

if (-not (Test-Path $resolvedImage)) {
    throw "Image nicht gefunden: $resolvedImage. Erst mit corefs-mkfs-image.ps1 oder 'corefs mkfs-image' erzeugen."
}

if (-not $Background) {
    Invoke-CoreFs -CoreFsArgs @($command, $resolvedImage, $driveRoot)
    return
}

if (Test-Path $drivePath) {
    throw "Laufwerk $driveRoot existiert bereits. Bitte einen freien Buchstaben waehlen oder zuerst unmounten."
}

$resolvedLogDir = [System.IO.Path]::GetFullPath($LogDir)
New-Item -ItemType Directory -Force -Path $resolvedLogDir | Out-Null

$statePath = Get-CoreFsMountStatePath -DriveLetter $driveRoot -PidPath $PidPath
$stateDir = Split-Path -Parent $statePath
New-Item -ItemType Directory -Force -Path $stateDir | Out-Null

if (Test-Path $statePath) {
    $previous = Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json
    if ($previous.ProcessId -and (Get-Process -Id $previous.ProcessId -ErrorAction SilentlyContinue)) {
        throw "Fuer $driveRoot laeuft bereits ein CoreFS-Mount-Prozess mit PID $($previous.ProcessId)."
    }
}

$driveName = $driveRoot.TrimEnd(":")
$stdoutPath = Join-Path $resolvedLogDir "corefs-$driveName.stdout.log"
$stderrPath = Join-Path $resolvedLogDir "corefs-$driveName.stderr.log"
$mountProcess = $null

try {
    $mountProcess = Start-CoreFsProcess `
        -CoreFsArgs @($command, $resolvedImage, $driveRoot) `
        -StdoutPath $stdoutPath `
        -StderrPath $stderrPath `
        -PreferExecutable `
        -BuildIfMissing `
        -Hidden

    $deadline = (Get-Date).AddSeconds($ReadyTimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if ($mountProcess.HasExited) {
            throw "CoreFS-Mount-Prozess wurde beendet, bevor $driveRoot bereit war. Logs: $stdoutPath / $stderrPath"
        }
        if (Test-Path $drivePath) {
            break
        }
        Start-Sleep -Milliseconds 250
    }

    if (-not (Test-Path $drivePath)) {
        throw "CoreFS-Mount wurde nicht rechtzeitig unter $driveRoot bereit. Logs: $stdoutPath / $stderrPath"
    }

    [pscustomobject]@{
        ProcessId = $mountProcess.Id
        DriveLetter = $driveRoot
        ImagePath = $resolvedImage
        ReadWrite = [bool]$ReadWrite
        StartedAt = (Get-Date).ToString("o")
        StdoutPath = $stdoutPath
        StderrPath = $stderrPath
    } | ConvertTo-Json | Set-Content -LiteralPath $statePath -Encoding UTF8

    Write-Host "CoreFS ist im Hintergrund gemountet: $driveRoot -> $resolvedImage"
    Write-Host "PID: $($mountProcess.Id)"
    Write-Host "Statusdatei: $statePath"
    Write-Host "Unmount: .\scripts\windows\corefs-unmount-image.ps1 -DriveLetter $driveRoot"
}
catch {
    if ($mountProcess -and -not $mountProcess.HasExited) {
        Stop-Process -Id $mountProcess.Id -Force
    }
    throw
}
