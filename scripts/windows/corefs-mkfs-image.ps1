param(
    [Parameter(Position = 0)]
    [Alias("Path", "LiteralPath")]
    [string]$ImagePath = ".\corefs-volume.img",
    [string]$Profile = "default",
    [switch]$Demo
)

. (Join-Path $PSScriptRoot "corefs-common.ps1")

$resolvedImage = Resolve-CoreFsUserPath -Path $ImagePath
$imageDir = Split-Path -Parent $resolvedImage
if ($imageDir) {
    New-Item -ItemType Directory -Force -Path $imageDir | Out-Null
}

$args = @("mkfs-image", $resolvedImage)
if ($Demo) {
    $args += "--demo"
}
if (-not [string]::IsNullOrWhiteSpace($Profile)) {
    $args += @("--profile", $Profile)
}

Invoke-CoreFs -CoreFsArgs $args
