param(
    [string]$EvidenceDir = "$PSScriptRoot\evidence",
    [switch]$Release
)

$ErrorActionPreference = "Stop"
$env:COREFS_CERT_EVIDENCE_DIR = [System.IO.Path]::GetFullPath($EvidenceDir)

if ($Release) {
    cargo test -p corefs-certification --release -- --nocapture
} else {
    cargo test -p corefs-certification -- --nocapture
}
