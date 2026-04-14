param(
    [string]$ImagePath = ".\corefs-volume.img",
    [string]$DriveLetter = "X:",
    [switch]$ReadWrite,
    [string]$Staging
)

. (Join-Path $PSScriptRoot "corefs-common.ps1")

$command = if ($ReadWrite) { "mount-image-rw" } else { "mount-image" }
$args = @($command, $ImagePath, $DriveLetter)

if ($Staging) {
    $args += @("--staging", $Staging)
}

Invoke-CoreFs -CoreFsArgs $args
