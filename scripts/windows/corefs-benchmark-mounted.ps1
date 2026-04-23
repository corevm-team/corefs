param(
    [string]$ImagePath = ".\corefs-bench.img",
    [string]$DriveLetter = "X:",
    [string]$LogPath = ".\PERFORMANCE_LOG.windows-mount.md",
    [string]$ResultsPath = "",
    [int]$Files = 128,
    [int]$PayloadBytes = 4096,
    [int]$SequentialMiB = 32,
    [int]$ReadyTimeoutSeconds = 30,
    [int]$MaxSeconds = 60,
    [string]$Profile = "performance",
    [ValidateSet("strict", "deferred")]
    [string]$FlushMode = "deferred",
    [string]$HistoryLabel = "windows-mount",
    [string]$HistoryDir = "",
    [switch]$NoPerfHistory
)

. (Join-Path $PSScriptRoot "corefs-common.ps1")

Assert-CoreFsWinFspInstalled
$budgetWatch = [System.Diagnostics.Stopwatch]::StartNew()

function Assert-MountBenchmarkBudget {
    param([string]$Stage)

    if ($budgetWatch.Elapsed.TotalSeconds -ge $MaxSeconds) {
        throw "Benchmark-Zeitbudget von ${MaxSeconds}s vor '$Stage' erreicht."
    }
}

function Normalize-DriveRoot {
    param([string]$Value)

    $trimmed = $Value.Trim().TrimEnd("\").TrimEnd("/")
    if ($trimmed.EndsWith(":")) {
        return $trimmed.ToUpperInvariant()
    }
    if ($trimmed.Length -eq 1) {
        return ($trimmed + ":").ToUpperInvariant()
    }
    throw "Invalid Windows drive letter: $Value"
}

function Append-MountBenchmarkRow {
    param(
        [string]$Path,
        [int]$Files,
        [int]$PayloadBytes,
        [int]$SequentialMiB,
        [double]$CreateMs,
        [double]$ReadMs,
        [double]$SequentialWriteMs,
        [double]$SequentialReadMs,
        [double]$DeleteMs
    )

    if (-not (Test-Path $Path)) {
        @(
            "# CoreFS Windows Mount Performance Log",
            "",
            "CoreFS profile: $Profile",
            "CoreFS WinFSP flush mode: $FlushMode",
            "",
            "| Timestamp | Mode | Files | Payload (B) | Seq MiB | Create (ms) | Read (ms) | Seq Write (ms) | Seq Read (ms) | Delete (ms) |",
            "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
        ) | Set-Content -Path $Path -Encoding UTF8
    }

    $timestamp = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd HH:mm:ss UTC")
    $row = "| $timestamp | winfsp-mounted/$Profile/$FlushMode | $Files | $PayloadBytes | $SequentialMiB | $([int]$CreateMs) | $([int]$ReadMs) | $([int]$SequentialWriteMs) | $([int]$SequentialReadMs) | $([int]$DeleteMs) |"
    Add-Content -Path $Path -Value $row -Encoding UTF8
}

function Convert-ToSafeDurationMs {
    param([double]$Milliseconds)

    if ($Milliseconds -lt 1.0) {
        return 1.0
    }
    return $Milliseconds
}

function Format-Metric {
    param(
        [double]$Value,
        [string]$Unit
    )

    return ("{0:N2} {1}" -f $Value, $Unit)
}

function Write-MountResultRow {
    param(
        [string]$Path,
        [string]$FsName,
        [string]$Workload,
        [double]$Milliseconds,
        [string]$Metric
    )

    $ms = [int][Math]::Round((Convert-ToSafeDurationMs $Milliseconds))
    "$FsName`t$Workload`t$ms`t$Metric" | Add-Content -Path $Path -Encoding UTF8
}

$driveRoot = Normalize-DriveRoot -Value $DriveLetter
$drivePath = "$driveRoot\"
$resolvedImage = [System.IO.Path]::GetFullPath($ImagePath)
$resolvedLog = [System.IO.Path]::GetFullPath($LogPath)
$imageDir = Split-Path -Parent $resolvedImage
$logDir = Split-Path -Parent $resolvedLog
if ($imageDir) {
    New-Item -ItemType Directory -Force -Path $imageDir | Out-Null
}
if ($logDir) {
    New-Item -ItemType Directory -Force -Path $logDir | Out-Null
}
$repoRoot = Get-CoreFsRepoRoot
$mountStdout = Join-Path $repoRoot "target\corefs-winfsp-mount.stdout.log"
$mountStderr = Join-Path $repoRoot "target\corefs-winfsp-mount.stderr.log"
if ([string]::IsNullOrWhiteSpace($ResultsPath)) {
    $ResultsPath = Join-Path $repoRoot "target\windows-bench\mounted-results.tsv"
}
$resolvedResults = [System.IO.Path]::GetFullPath($ResultsPath)
$resultsDir = Split-Path -Parent $resolvedResults
if ($resultsDir) {
    New-Item -ItemType Directory -Force -Path $resultsDir | Out-Null
}
"fs`tworkload`tms`tmetric" | Set-Content -Path $resolvedResults -Encoding UTF8

if (Test-Path $drivePath) {
    throw "Drive $driveRoot already exists. Choose a free drive letter."
}

Invoke-CoreFs -CoreFsArgs @("mkfs-image", $resolvedImage, "--demo", "--profile", $Profile)

$mountProcess = $null
$previousFlushMode = $env:COREFS_WINDOWS_FLUSH_MODE
try {
    $env:COREFS_WINDOWS_FLUSH_MODE = $FlushMode
    $mountProcess = Start-CoreFsProcess `
        -CoreFsArgs @("mount-image-rw", $resolvedImage, $driveRoot) `
        -StdoutPath $mountStdout `
        -StderrPath $mountStderr `
        -PreferExecutable `
        -BuildIfMissing `
        -Hidden

    $deadline = (Get-Date).AddSeconds($ReadyTimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path $drivePath) {
            break
        }
        Start-Sleep -Milliseconds 250
    }

    if (-not (Test-Path $drivePath)) {
        throw "CoreFS mount did not become ready at $driveRoot. See $mountStdout and $mountStderr."
    }

    $benchDir = Join-Path $drivePath ".corefs-bench"
    if (Test-Path $benchDir) {
        Remove-CoreFsTreeRobust -Path $benchDir
    }
    New-Item -ItemType Directory -Force -Path $benchDir | Out-Null

    $payload = New-Object byte[] $PayloadBytes
    for ($i = 0; $i -lt $payload.Length; $i++) {
        $payload[$i] = 120
    }

    Assert-MountBenchmarkBudget -Stage "create"
    $create = Measure-Command {
        for ($i = 1; $i -le $Files; $i++) {
            [System.IO.File]::WriteAllBytes((Join-Path $benchDir ("file-{0:D5}.bin" -f $i)), $payload)
        }
    }

    Assert-MountBenchmarkBudget -Stage "read"
    $read = Measure-Command {
        for ($i = 1; $i -le $Files; $i++) {
            [void][System.IO.File]::ReadAllBytes((Join-Path $benchDir ("file-{0:D5}.bin" -f $i)))
        }
    }

    $seqPath = Join-Path $benchDir "sequential.bin"
    $chunk = New-Object byte[] (1024 * 1024)
    Assert-MountBenchmarkBudget -Stage "sequential write"
    $seqWrite = Measure-Command {
        $stream = [System.IO.File]::Open($seqPath, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
        try {
            for ($i = 0; $i -lt $SequentialMiB; $i++) {
                $stream.Write($chunk, 0, $chunk.Length)
            }
            $stream.Flush($true)
        }
        finally {
            $stream.Dispose()
        }
    }

    Assert-MountBenchmarkBudget -Stage "sequential read"
    $seqRead = Measure-Command {
        $stream = [System.IO.File]::OpenRead($seqPath)
        try {
            $buffer = New-Object byte[] (1024 * 1024)
            while ($stream.Read($buffer, 0, $buffer.Length) -gt 0) {}
        }
        finally {
            $stream.Dispose()
        }
    }

    Assert-MountBenchmarkBudget -Stage "delete"
    $delete = Measure-Command {
        Remove-CoreFsTreeRobust -Path $benchDir
    }

    Append-MountBenchmarkRow `
        -Path $resolvedLog `
        -Files $Files `
        -PayloadBytes $PayloadBytes `
        -SequentialMiB $SequentialMiB `
        -CreateMs $create.TotalMilliseconds `
        -ReadMs $read.TotalMilliseconds `
        -SequentialWriteMs $seqWrite.TotalMilliseconds `
        -SequentialReadMs $seqRead.TotalMilliseconds `
        -DeleteMs $delete.TotalMilliseconds

    $createMs = Convert-ToSafeDurationMs $create.TotalMilliseconds
    $readMs = Convert-ToSafeDurationMs $read.TotalMilliseconds
    $seqWriteMs = Convert-ToSafeDurationMs $seqWrite.TotalMilliseconds
    $seqReadMs = Convert-ToSafeDurationMs $seqRead.TotalMilliseconds
    $deleteMs = Convert-ToSafeDurationMs $delete.TotalMilliseconds

    Write-MountResultRow $resolvedResults "corefs-winfsp" "create_${Files}x${PayloadBytes}B" $createMs (Format-Metric (($Files * 1000.0) / $createMs) "ops/s")
    Write-MountResultRow $resolvedResults "corefs-winfsp" "read_${Files}x${PayloadBytes}B" $readMs (Format-Metric (($Files * 1000.0) / $readMs) "ops/s")
    Write-MountResultRow $resolvedResults "corefs-winfsp" "seq_write_${SequentialMiB}MiB" $seqWriteMs (Format-Metric (($SequentialMiB * 1000.0) / $seqWriteMs) "MiB/s")
    Write-MountResultRow $resolvedResults "corefs-winfsp" "seq_read_${SequentialMiB}MiB" $seqReadMs (Format-Metric (($SequentialMiB * 1000.0) / $seqReadMs) "MiB/s")
    Write-MountResultRow $resolvedResults "corefs-winfsp" "delete_${Files}_small" $deleteMs "-"

    $historyPath = $null
    if (-not $NoPerfHistory) {
        $historyPath = Publish-CoreFsPerfHistory -SourcePath $resolvedResults -Label $HistoryLabel -HistoryDir $HistoryDir
    }

    Write-Host "Mounted benchmark log written to $resolvedLog"
    Write-Host "Mounted benchmark TSV written to $resolvedResults"
    if ($historyPath) {
        Write-Host "Perf history: $historyPath"
    }
}
finally {
    if ($null -eq $previousFlushMode) {
        Remove-Item Env:\COREFS_WINDOWS_FLUSH_MODE -ErrorAction SilentlyContinue
    } else {
        $env:COREFS_WINDOWS_FLUSH_MODE = $previousFlushMode
    }
    if ($mountProcess -and -not $mountProcess.HasExited) {
        if ($FlushMode -eq "deferred") {
            Start-Sleep -Milliseconds 1200
        }
        Stop-Process -Id $mountProcess.Id -Force
        $mountProcess.WaitForExit()
    }
}
