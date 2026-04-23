param(
    [string]$DriveLetter = "X:",
    [string]$PidPath = "",
    [int]$TimeoutSeconds = 15
)

. (Join-Path $PSScriptRoot "corefs-common.ps1")

$driveRoot = Normalize-CoreFsDriveRoot -Value $DriveLetter
$drivePath = "$driveRoot\"
$statePath = Get-CoreFsMountStatePath -DriveLetter $driveRoot -PidPath $PidPath

if (-not (Test-Path $statePath)) {
    Write-Host "Keine CoreFS-Hintergrund-Mount-Statusdatei gefunden: $statePath"
    Write-Host "Falls $driveRoot in einem Vordergrundfenster gemountet wurde, dort Ctrl+C druecken."
    Write-Host "CoreFS nutzt native WinFSP-Mounts, kein subst."
    return
}

$state = Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json
$process = $null
if ($state.ProcessId) {
    $process = Get-Process -Id $state.ProcessId -ErrorAction SilentlyContinue
}

if ($process) {
    Write-Host "Stoppe CoreFS-Mount $driveRoot (PID $($process.Id))..."
    Stop-Process -Id $process.Id -Force

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if (-not (Get-Process -Id $process.Id -ErrorAction SilentlyContinue)) {
            break
        }
        Start-Sleep -Milliseconds 250
    }
}
else {
    Write-Host "Der gespeicherte CoreFS-Prozess laeuft nicht mehr: PID $($state.ProcessId)"
}

$deadline = (Get-Date).AddSeconds($TimeoutSeconds)
while ((Get-Date) -lt $deadline) {
    if (-not (Test-Path $drivePath)) {
        break
    }
    Start-Sleep -Milliseconds 250
}

Remove-Item -LiteralPath $statePath -Force

if (Test-Path $drivePath) {
    Write-Host "Hinweis: $driveRoot ist noch sichtbar. Windows kann ein paar Sekunden brauchen, bis WinFSP den Mountpoint entfernt."
}
else {
    Write-Host "CoreFS-Mount entfernt: $driveRoot"
}
