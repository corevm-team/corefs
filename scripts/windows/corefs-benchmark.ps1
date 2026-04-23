param(
    [string]$LogPath = ".\PERFORMANCE_LOG.windows.md",
    [string[]]$Profiles = @("balanced", "small-files", "metadata-heavy", "snapshot-heavy", "persist-heavy"),
    [int]$Files = 0,
    [int]$PayloadBytes = 0,
    [int]$Snapshots = 0,
    [int]$Saves = 0
)

. (Join-Path $PSScriptRoot "corefs-common.ps1")

$resolvedLog = [System.IO.Path]::GetFullPath($LogPath)

foreach ($profile in $Profiles) {
    $args = @("benchmark-log", $resolvedLog, "--profile", $profile)

    if ($Files -gt 0) {
        $args += @("--files", $Files.ToString())
    }
    if ($PayloadBytes -gt 0) {
        $args += @("--payload", $PayloadBytes.ToString())
    }
    if ($Snapshots -gt 0) {
        $args += @("--snapshots", $Snapshots.ToString())
    }
    if ($Saves -gt 0) {
        $args += @("--saves", $Saves.ToString())
    }

    Write-Host "Running CoreFS benchmark profile '$profile'..."
    Invoke-CoreFs -CoreFsArgs $args
}

Write-Host "Benchmark log written to $resolvedLog"
