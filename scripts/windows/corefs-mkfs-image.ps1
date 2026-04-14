param(
    [string]$ImagePath = ".\corefs-volume.img",
    [switch]$Demo
)

. (Join-Path $PSScriptRoot "corefs-common.ps1")

$args = @("mkfs-image", $ImagePath)
if ($Demo) {
    $args += "--demo"
}

Invoke-CoreFs -CoreFsArgs $args
