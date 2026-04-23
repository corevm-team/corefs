param(
    [string]$ImagePath = ".\corefs-volume.img",
    [string]$Profile = "default",
    [switch]$Demo
)

. (Join-Path $PSScriptRoot "corefs-common.ps1")

$args = @("mkfs-image", $ImagePath)
if ($Demo) {
    $args += "--demo"
}
if (-not [string]::IsNullOrWhiteSpace($Profile)) {
    $args += @("--profile", $Profile)
}

Invoke-CoreFs -CoreFsArgs $args
