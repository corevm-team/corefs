$ErrorActionPreference = "Stop"

function Get-CoreFsRepoRoot {
    param(
        [string]$StartDir = $PSScriptRoot
    )

    return [System.IO.Path]::GetFullPath((Join-Path $StartDir "..\.."))
}

function Get-CoreFsCommand {
    param(
        [string]$RepoRoot,
        [switch]$PreferExecutable,
        [switch]$BuildIfMissing
    )

    $distBin = Join-Path $RepoRoot "dist\bin\corefs.exe"
    $targetBin = Join-Path $RepoRoot "target\release\corefs.exe"

    if ($PreferExecutable) {
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

        if ($BuildIfMissing -and (Get-Command cargo -ErrorAction SilentlyContinue)) {
            Push-Location $RepoRoot
            try {
                & cargo build --release --features windows-winfsp --bin corefs
                if ($LASTEXITCODE -ne 0) {
                    exit $LASTEXITCODE
                }
            }
            finally {
                Pop-Location
            }

            if (Test-Path $targetBin) {
                return @{
                    FilePath = $targetBin
                    Arguments = @()
                    WorkingDirectory = $RepoRoot
                }
            }
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

    if (Get-Command cargo -ErrorAction SilentlyContinue) {
        return @{
            FilePath = "cargo"
            Arguments = @("run", "--release", "--features", "windows-winfsp", "--bin", "corefs", "--")
            WorkingDirectory = $RepoRoot
        }
    }

    throw "Keine CoreFS-CLI gefunden. Bitte 'cargo build --release --features windows-winfsp --bin corefs' ausfuehren oder dist/bin/corefs.exe bereitstellen."
}

function Normalize-CoreFsDriveRoot {
    param([string]$Value)

    $trimmed = $Value.Trim().TrimEnd("\").TrimEnd("/")
    if ($trimmed.EndsWith(":") -and $trimmed.Length -eq 2) {
        return $trimmed.ToUpperInvariant()
    }
    if ($trimmed.Length -eq 1) {
        return ($trimmed + ":").ToUpperInvariant()
    }
    throw "Ungueltiger Windows-Laufwerksbuchstabe: $Value"
}

function Get-CoreFsMountStatePath {
    param(
        [string]$DriveLetter,
        [string]$PidPath = ""
    )

    if ($PidPath) {
        return [System.IO.Path]::GetFullPath($PidPath)
    }

    $repoRoot = Get-CoreFsRepoRoot
    $stateDir = Join-Path $repoRoot "target\windows-mounts"
    $driveRoot = Normalize-CoreFsDriveRoot -Value $DriveLetter
    $driveName = $driveRoot.TrimEnd(":")
    return (Join-Path $stateDir "corefs-$driveName.mount.json")
}

function Test-CoreFsWinFspInstalled {
    $dllCandidates = @(
        "$env:WINDIR\System32\winfsp-x64.dll",
        "$env:WINDIR\System32\winfsp-a64.dll",
        "$env:WINDIR\System32\winfsp-x86.dll"
    )

    foreach ($candidate in $dllCandidates) {
        if (Test-Path $candidate) {
            return $true
        }
    }

    $sxsCandidates = @(
        "$env:ProgramFiles\WinFsp\SxS",
        "${env:ProgramFiles(x86)}\WinFsp\SxS"
    )

    foreach ($dir in $sxsCandidates) {
        if ($dir -and (Test-Path $dir)) {
            $dll = Get-ChildItem -Path $dir -Recurse -Filter "winfsp-*.dll" -ErrorAction SilentlyContinue | Select-Object -First 1
            if ($dll) {
                return $true
            }
        }
    }

    $programDirs = @(
        "$env:ProgramFiles\WinFsp",
        "${env:ProgramFiles(x86)}\WinFsp"
    )

    foreach ($dir in $programDirs) {
        if ($dir -and (Test-Path $dir)) {
            return $true
        }
    }

    return $false
}

function Assert-CoreFsWinFspInstalled {
    if (-not (Test-CoreFsWinFspInstalled)) {
        throw "WinFSP 2.x Runtime nicht gefunden. Bitte WinFSP installieren; CoreFS nutzt native WinFSP-Laufwerke, kein subst."
    }
}

function Join-CoreFsArgumentList {
    param(
        [string[]]$Arguments
    )

    $quoted = foreach ($arg in $Arguments) {
        if ($arg -match '[\s"]') {
            '"' + ($arg -replace '"', '\"') + '"'
        }
        else {
            $arg
        }
    }

    return ($quoted -join " ")
}

function Start-CoreFsProcess {
    param(
        [string[]]$CoreFsArgs,
        [string]$StdoutPath,
        [string]$StderrPath,
        [switch]$PreferExecutable,
        [switch]$BuildIfMissing,
        [switch]$Hidden
    )

    $repoRoot = Get-CoreFsRepoRoot
    $cmd = Get-CoreFsCommand -RepoRoot $repoRoot -PreferExecutable:$PreferExecutable -BuildIfMissing:$BuildIfMissing
    $allArgs = @($cmd.Arguments + $CoreFsArgs)

    $startArgs = @{
        FilePath = $cmd.FilePath
        ArgumentList = (Join-CoreFsArgumentList -Arguments $allArgs)
        WorkingDirectory = $cmd.WorkingDirectory
        PassThru = $true
    }

    if ($StdoutPath) {
        $startArgs.RedirectStandardOutput = $StdoutPath
    }
    if ($StderrPath) {
        $startArgs.RedirectStandardError = $StderrPath
    }
    if ($Hidden) {
        $startArgs.WindowStyle = "Hidden"
    }

    return Start-Process @startArgs
}

function Remove-CoreFsTreeRobust {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }

    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer) {
        Remove-Item -LiteralPath $Path -Force -ErrorAction Stop
        return
    }

    $children = @(Get-ChildItem -LiteralPath $Path -Force)
    foreach ($child in $children) {
        Remove-CoreFsTreeRobust -Path $child.FullName
    }

    try {
        Remove-Item -LiteralPath $Path -Force -ErrorAction Stop
    }
    catch {
        if (-not (Test-Path -LiteralPath $Path)) {
            return
        }

        $remaining = @(Get-ChildItem -LiteralPath $Path -Force -ErrorAction SilentlyContinue)
        if ($remaining.Count -eq 0) {
            return
        }

        throw
    }
}

function Remove-CoreFsTreeBestEffort {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [int]$Retries = 5,
        [int]$DelayMs = 120
    )

    function Test-BenignCoreFsDeleteError {
        param([object]$ErrorRecord)

        if (-not $ErrorRecord -or -not $ErrorRecord.Exception) {
            return $false
        }

        $message = $ErrorRecord.Exception.Message
        return ($message -match "nicht finden" `
            -or $message -match "cannot find" `
            -or $message -match "Could not find" `
            -or $message -match "not found")
    }

    $lastError = $null
    for ($attempt = 1; $attempt -le $Retries; $attempt++) {
        if (-not (Test-Path -LiteralPath $Path)) {
            return
        }

        try {
            Remove-CoreFsTreeRobust -Path $Path
            return
        }
        catch {
            $lastError = $_
            if (Test-BenignCoreFsDeleteError -ErrorRecord $lastError -and -not (Test-Path -LiteralPath $Path)) {
                return
            }
        }

        try {
            if (Test-Path -LiteralPath $Path) {
                Remove-Item -LiteralPath $Path -Recurse -Force -ErrorAction Stop
            }
            return
        }
        catch {
            $lastError = $_
            if (Test-BenignCoreFsDeleteError -ErrorRecord $lastError -and -not (Test-Path -LiteralPath $Path)) {
                return
            }
        }

        Start-Sleep -Milliseconds $DelayMs
    }

    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }

    $remaining = @(Get-ChildItem -LiteralPath $Path -Force -ErrorAction SilentlyContinue)
    if ($remaining.Count -eq 0) {
        return
    }

    if ($lastError) {
        throw $lastError
    }
    throw "Failed to remove $Path"
}

function Get-CoreFsPerfHistoryDir {
    param(
        [string]$HistoryDir = ""
    )

    if ([string]::IsNullOrWhiteSpace($HistoryDir)) {
        $HistoryDir = Join-Path (Get-CoreFsRepoRoot) "perf-history"
    }

    $resolved = [System.IO.Path]::GetFullPath($HistoryDir)
    New-Item -ItemType Directory -Force -Path $resolved | Out-Null
    return $resolved
}

function New-CoreFsPerfHistoryPath {
    param(
        [string]$Label,
        [string]$HistoryDir = ""
    )

    $safeLabel = ($Label -replace '[^A-Za-z0-9_.-]', '-').Trim("-")
    if ([string]::IsNullOrWhiteSpace($safeLabel)) {
        $safeLabel = "windows"
    }

    $dir = Get-CoreFsPerfHistoryDir -HistoryDir $HistoryDir
    $stamp = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd_HHmmss")
    return (Join-Path $dir "${stamp}_${safeLabel}.tsv")
}

function Publish-CoreFsPerfHistory {
    param(
        [Parameter(Mandatory = $true)]
        [string]$SourcePath,
        [string]$Label,
        [string]$HistoryDir = ""
    )

    if (-not (Test-Path -LiteralPath $SourcePath)) {
        throw "Performance result source not found: $SourcePath"
    }

    $historyPath = New-CoreFsPerfHistoryPath -Label $Label -HistoryDir $HistoryDir
    Copy-Item -LiteralPath $SourcePath -Destination $historyPath -Force
    return $historyPath
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
