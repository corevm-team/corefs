param(
    [string]$DriveLetter = "X:",
    [switch]$Discard
)

. (Join-Path $PSScriptRoot "corefs-common.ps1")

$args = @("unmount-image-win", $DriveLetter)
if ($Discard) {
    $args += "--discard"
}

Invoke-CoreFs -CoreFsArgs $args
