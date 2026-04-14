$ErrorActionPreference = "Stop"

function Get-CoreFsRepoRoot {
    param(
        [string]$StartDir = $PSScriptRoot
    )

    return [System.IO.Path]::GetFullPath((Join-Path $StartDir "..\.."))
}

function Get-CoreFsCommand {
    param(
        [string]$RepoRoot
    )

    $distBin = Join-Path $RepoRoot "dist\bin\corefs.exe"
    $targetBin = Join-Path $RepoRoot "target\release\corefs.exe"

    if (Get-Command cargo -ErrorAction SilentlyContinue) {
        return @{
            FilePath = "cargo"
            Arguments = @("run", "--release", "--")
            WorkingDirectory = $RepoRoot
        }
    }

    if (Test-Path $distBin) {
        return @{
            FilePath = $distBin
            Arguments = @()
            WorkingDirectory = $RepoRoot
        }
    }

    if (Test-Path $targetBin) {
        return @{
            FilePath = $targetBin
            Arguments = @()
            WorkingDirectory = $RepoRoot
        }
    }

    throw "Keine CoreFS-CLI gefunden. Bitte 'cargo build --release' ausfuehren oder dist/bin/corefs.exe bereitstellen."
}

function Invoke-CoreFs {
    param(
        [string[]]$CoreFsArgs
    )

    $repoRoot = Get-CoreFsRepoRoot
    $cmd = Get-CoreFsCommand -RepoRoot $repoRoot
    $allArgs = @($cmd.Arguments + $CoreFsArgs)

    Push-Location $cmd.WorkingDirectory
    try {
        & $cmd.FilePath @allArgs
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
    }
    finally {
        Pop-Location
    }
}
