param(
    [string]$ImagePath = ".\target\windows-bench\corefs.img",
    [string]$DriveLetter = "X:",
    [string]$WorkDir = ".\target\windows-bench",
    [string]$ResultsPath = "",
    [string]$MarkdownPath = ".\target\PERFORMANCE_LOG.windows-vs-ntfs.md",
    [int]$Files = 64,
    [int]$PayloadBytes = 4096,
    [int]$SequentialMiB = 32,
    [int]$FsyncFiles = 8,
    [int]$RandomMiB = 8,
    [int]$RandomOps = 128,
    [int]$AppendOps = 256,
    [int]$BigDirFiles = 512,
    [int]$ReadyTimeoutSeconds = 30,
    [int]$MaxSeconds = 60,
    [string]$Profile = "performance",
    [ValidateSet("strict", "deferred")]
    [string]$FlushMode = "deferred",
    [string]$HistoryLabel = "windows-vs-ntfs",
    [string]$HistoryDir = "",
    [switch]$NoPerfHistory
)

. (Join-Path $PSScriptRoot "corefs-common.ps1")

Assert-CoreFsWinFspInstalled
$budgetWatch = [System.Diagnostics.Stopwatch]::StartNew()

function Test-BenchmarkBudget {
    return ($budgetWatch.Elapsed.TotalSeconds -lt $MaxSeconds)
}

function Assert-BenchmarkBudget {
    param([string]$Stage)

    if (-not (Test-BenchmarkBudget)) {
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

function Write-ResultRow {
    param(
        [string]$Path,
        [string]$FsName,
        [string]$Workload,
        [double]$Milliseconds,
        [string]$Metric
    )

    $ms = [int][Math]::Round($Milliseconds)
    "$FsName`t$Workload`t$ms`t$Metric" | Add-Content -Path $Path -Encoding UTF8
    Write-Host ("  {0,-16} {1,-28} {2,8} ms   {3}" -f $FsName, $Workload, $ms, $Metric)
}

function Invoke-CoreFsWindowsWorkload {
    param(
        [string]$FsName,
        [string]$RootPath,
        [string]$ResultsPath,
        [int]$Files,
        [int]$PayloadBytes,
        [int]$SequentialMiB,
        [int]$FsyncFiles,
        [int]$RandomMiB,
        [int]$RandomOps,
        [int]$AppendOps,
        [int]$BigDirFiles,
        [switch]$EnforceBudget
    )

    $benchDir = Join-Path $RootPath "bench"
    if (Test-Path $benchDir) {
        Remove-CoreFsTreeRobust -Path $benchDir
    }
    New-Item -ItemType Directory -Force -Path $benchDir | Out-Null

    $payload = New-Object byte[] $PayloadBytes
    for ($i = 0; $i -lt $payload.Length; $i++) {
        $payload[$i] = 120
    }

    if ($EnforceBudget) { Assert-BenchmarkBudget "$FsName create" }
    $create = Measure-Command {
        for ($i = 0; $i -lt $Files; $i++) {
            [System.IO.File]::WriteAllBytes((Join-Path $benchDir ("f{0:D5}.bin" -f $i)), $payload)
        }
    }
    $createMs = Convert-ToSafeDurationMs $create.TotalMilliseconds
    Write-ResultRow $ResultsPath $FsName "create_${Files}x${PayloadBytes}B" $createMs (Format-Metric (($Files * 1000.0) / $createMs) "ops/s")

    if ($EnforceBudget) { Assert-BenchmarkBudget "$FsName read" }
    $read = Measure-Command {
        for ($i = 0; $i -lt $Files; $i++) {
            [void][System.IO.File]::ReadAllBytes((Join-Path $benchDir ("f{0:D5}.bin" -f $i)))
        }
    }
    $readMs = Convert-ToSafeDurationMs $read.TotalMilliseconds
    Write-ResultRow $ResultsPath $FsName "read_${Files}x${PayloadBytes}B" $readMs (Format-Metric (($Files * 1000.0) / $readMs) "ops/s")

    if ($EnforceBudget) { Assert-BenchmarkBudget "$FsName stat" }
    $stat = Measure-Command {
        [void](Get-ChildItem -LiteralPath $benchDir -Force)
    }
    Write-ResultRow $ResultsPath $FsName "stat_list_${Files}" (Convert-ToSafeDurationMs $stat.TotalMilliseconds) "-"

    $fsyncColdDir = Join-Path $benchDir "fsync-cold"
    New-Item -ItemType Directory -Path $fsyncColdDir | Out-Null
    if ($EnforceBudget) { Assert-BenchmarkBudget "$FsName fsync cold" }
    $fsyncCold = Measure-Command {
        for ($i = 0; $i -lt $FsyncFiles; $i++) {
            $path = Join-Path $fsyncColdDir ("f{0:D5}.bin" -f $i)
            $stream = [System.IO.File]::Open($path, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
            try {
                $stream.Write($payload, 0, $payload.Length)
                $stream.Flush($true)
            }
            finally {
                $stream.Dispose()
            }
        }
    }
    $fsyncColdMs = Convert-ToSafeDurationMs $fsyncCold.TotalMilliseconds
    Write-ResultRow $ResultsPath $FsName "fsync_cold_${FsyncFiles}x${PayloadBytes}B" $fsyncColdMs (Format-Metric (($FsyncFiles * 1000.0) / $fsyncColdMs) "ops/s")
    Remove-CoreFsTreeRobust -Path $fsyncColdDir

    $seqPath = Join-Path $benchDir "seq.bin"
    $chunk = New-Object byte[] (1024 * 1024)
    if ($EnforceBudget) { Assert-BenchmarkBudget "$FsName sequential write" }
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
    $seqWriteMs = Convert-ToSafeDurationMs $seqWrite.TotalMilliseconds
    Write-ResultRow $ResultsPath $FsName "seq_write_${SequentialMiB}MiB" $seqWriteMs (Format-Metric (($SequentialMiB * 1000.0) / $seqWriteMs) "MiB/s")

    if ($EnforceBudget) { Assert-BenchmarkBudget "$FsName sequential read" }
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
    $seqReadMs = Convert-ToSafeDurationMs $seqRead.TotalMilliseconds
    Write-ResultRow $ResultsPath $FsName "seq_read_${SequentialMiB}MiB" $seqReadMs (Format-Metric (($SequentialMiB * 1000.0) / $seqReadMs) "MiB/s")

    $fsyncDir = Join-Path $benchDir "fsync"
    New-Item -ItemType Directory -Path $fsyncDir | Out-Null
    if ($EnforceBudget) { Assert-BenchmarkBudget "$FsName fsync" }
    $fsync = Measure-Command {
        for ($i = 0; $i -lt $FsyncFiles; $i++) {
            $path = Join-Path $fsyncDir ("f{0:D5}.bin" -f $i)
            $stream = [System.IO.File]::Open($path, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
            try {
                $stream.Write($payload, 0, $payload.Length)
                $stream.Flush($true)
            }
            finally {
                $stream.Dispose()
            }
        }
    }
    $fsyncMs = Convert-ToSafeDurationMs $fsync.TotalMilliseconds
    Write-ResultRow $ResultsPath $FsName "fsync_${FsyncFiles}x${PayloadBytes}B" $fsyncMs (Format-Metric (($FsyncFiles * 1000.0) / $fsyncMs) "ops/s")

    $randPath = Join-Path $benchDir "rand.bin"
    $randSize = $RandomMiB * 1024 * 1024
    $randBuffer = New-Object byte[] 4096
    [System.IO.File]::WriteAllBytes($randPath, (New-Object byte[] $randSize))
    $random = New-Object System.Random 42
    if ($EnforceBudget) { Assert-BenchmarkBudget "$FsName random" }
    $rand = Measure-Command {
        $stream = [System.IO.File]::Open($randPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None)
        try {
            for ($i = 0; $i -lt $RandomOps; $i++) {
                $offset = [int64]($random.Next(0, [Math]::Max(1, $randSize - 4096)) -band (-bnot 4095))
                $stream.Position = $offset
                if ($random.NextDouble() -lt 0.7) {
                    [void]$stream.Read($randBuffer, 0, $randBuffer.Length)
                }
                else {
                    $stream.Write($randBuffer, 0, $randBuffer.Length)
                }
            }
            $stream.Flush($true)
        }
        finally {
            $stream.Dispose()
        }
    }
    $randMs = Convert-ToSafeDurationMs $rand.TotalMilliseconds
    Write-ResultRow $ResultsPath $FsName "rand4k_${RandomOps}ops_70r30w" $randMs (Format-Metric (($RandomOps * 1000.0) / $randMs) "ops/s")

    if ($EnforceBudget) { Assert-BenchmarkBudget "$FsName delete" }
    $delete = Measure-Command {
        $filesToDelete = @(Get-ChildItem -LiteralPath $benchDir -Filter "f*.bin")
        foreach ($file in $filesToDelete) {
            Remove-Item -LiteralPath $file.FullName -Force
        }
        Remove-CoreFsTreeRobust -Path $fsyncDir
    }
    Write-ResultRow $ResultsPath $FsName "delete_${Files}_small" (Convert-ToSafeDurationMs $delete.TotalMilliseconds) "-"

    $bigDir = Join-Path $benchDir "bigdir"
    New-Item -ItemType Directory -Path $bigDir | Out-Null
    if ($EnforceBudget) { Assert-BenchmarkBudget "$FsName bigdir" }
    $bigDirCreate = Measure-Command {
        for ($i = 0; $i -lt $BigDirFiles; $i++) {
            [System.IO.File]::WriteAllBytes((Join-Path $bigDir ("f{0:D5}" -f $i)), (New-Object byte[] 0))
        }
    }
    $bigDirMs = Convert-ToSafeDurationMs $bigDirCreate.TotalMilliseconds
    Write-ResultRow $ResultsPath $FsName "bigdir_create_${BigDirFiles}" $bigDirMs (Format-Metric (($BigDirFiles * 1000.0) / $bigDirMs) "ops/s")
    Remove-CoreFsTreeRobust -Path $bigDir

    $appendPath = Join-Path $benchDir "log.bin"
    $appendRecord = New-Object byte[] 1024
    if ($EnforceBudget) { Assert-BenchmarkBudget "$FsName append" }
    $append = Measure-Command {
        $stream = [System.IO.File]::Open($appendPath, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
        try {
            for ($i = 0; $i -lt $AppendOps; $i++) {
                $stream.Write($appendRecord, 0, $appendRecord.Length)
            }
            $stream.Flush($true)
        }
        finally {
            $stream.Dispose()
        }
    }
    $appendMs = Convert-ToSafeDurationMs $append.TotalMilliseconds
    Write-ResultRow $ResultsPath $FsName "append_log_${AppendOps}x1KiB" $appendMs (Format-Metric (($AppendOps * 1000.0) / $appendMs) "ops/s")

    try {
        Remove-CoreFsTreeRobust -Path $benchDir
    }
    catch {
        Write-Host "  cleanup warning for ${FsName}: $($_.Exception.Message)"
    }
}

function Convert-TsvToMarkdown {
    param(
        [string]$TsvPath,
        [string]$MarkdownPath
    )

    $timestamp = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd HH:mm:ss UTC")
    $rows = @(Get-Content -Path $TsvPath | Select-Object -Skip 1 | ForEach-Object {
        $parts = $_ -split "`t"
        if ($parts.Length -ge 4) {
            [pscustomobject]@{
                Fs = $parts[0]
                Workload = $parts[1]
                Ms = [double]$parts[2]
                Metric = $parts[3]
            }
        }
    })

    $lines = @(
        "# CoreFS Windows vs NTFS Performance",
        "",
        "Timestamp: $timestamp",
        "",
        "Budget: ${MaxSeconds}s",
        "",
        "CoreFS profile: $Profile",
        "CoreFS WinFSP flush mode: $FlushMode",
        "",
        "| FS | Workload | ms | Metric |",
        "| --- | --- | ---: | --- |"
    )

    foreach ($row in $rows) {
        $lines += "| $($row.Fs) | $($row.Workload) | $([int]$row.Ms) | $($row.Metric) |"
    }

    $lines += ""
    $lines += "## Auswertung"
    $lines += ""
    $lines += "| Workload | CoreFS ms | NTFS ms | CoreFS langsamer |"
    $lines += "| --- | ---: | ---: | ---: |"

    $coreRows = $rows | Where-Object { $_.Fs -eq "corefs-winfsp" }
    foreach ($core in $coreRows) {
        $ntfs = $rows | Where-Object { $_.Fs -eq "ntfs-direct" -and $_.Workload -eq $core.Workload } | Select-Object -First 1
        if ($ntfs -and $ntfs.Ms -gt 0) {
            $slowdown = $core.Ms / $ntfs.Ms
            $lines += "| $($core.Workload) | $([int]$core.Ms) | $([int]$ntfs.Ms) | $('{0:N1}x' -f $slowdown) |"
        }
    }

    $lines | Set-Content -Path $MarkdownPath -Encoding UTF8
}

$driveRoot = Normalize-DriveRoot -Value $DriveLetter
$drivePath = "$driveRoot\"
$resolvedWorkDir = [System.IO.Path]::GetFullPath($WorkDir)
$resolvedImage = [System.IO.Path]::GetFullPath($ImagePath)
$resolvedMarkdown = [System.IO.Path]::GetFullPath($MarkdownPath)
if ([string]::IsNullOrWhiteSpace($ResultsPath)) {
    $ResultsPath = Join-Path $resolvedWorkDir "results.tsv"
}
$resolvedResults = [System.IO.Path]::GetFullPath($ResultsPath)
$repoRoot = Get-CoreFsRepoRoot
$mountStdout = Join-Path $repoRoot "target\corefs-winfsp-vs-ntfs.stdout.log"
$mountStderr = Join-Path $repoRoot "target\corefs-winfsp-vs-ntfs.stderr.log"
$ntfsRoot = Join-Path $resolvedWorkDir "ntfs-direct"

New-Item -ItemType Directory -Force -Path $resolvedWorkDir | Out-Null
"fs`tworkload`tms`tmetric" | Set-Content -Path $resolvedResults -Encoding UTF8

if (Test-Path $drivePath) {
    throw "Drive $driveRoot already exists. Choose a free drive letter."
}

Write-Host "[*] CoreFS: creating image at $resolvedImage"
Invoke-CoreFs -CoreFsArgs @("mkfs-image", $resolvedImage, "--demo", "--profile", $Profile)

$mountProcess = $null
$previousFlushMode = $env:COREFS_WINDOWS_FLUSH_MODE
try {
    Write-Host "[*] CoreFS: mounting read-write at $driveRoot"
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

    Invoke-CoreFsWindowsWorkload `
        -FsName "corefs-winfsp" `
        -RootPath $drivePath `
        -ResultsPath $resolvedResults `
        -Files $Files `
        -PayloadBytes $PayloadBytes `
        -SequentialMiB $SequentialMiB `
        -FsyncFiles $FsyncFiles `
        -RandomMiB $RandomMiB `
        -RandomOps $RandomOps `
        -AppendOps $AppendOps `
        -BigDirFiles $BigDirFiles `
        -EnforceBudget
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

Write-Host "[*] NTFS: direct host filesystem baseline at $ntfsRoot"
if (Test-Path $ntfsRoot) {
    Remove-CoreFsTreeRobust -Path $ntfsRoot
}
New-Item -ItemType Directory -Force -Path $ntfsRoot | Out-Null

Invoke-CoreFsWindowsWorkload `
    -FsName "ntfs-direct" `
    -RootPath $ntfsRoot `
    -ResultsPath $resolvedResults `
    -Files $Files `
    -PayloadBytes $PayloadBytes `
    -SequentialMiB $SequentialMiB `
    -FsyncFiles $FsyncFiles `
    -RandomMiB $RandomMiB `
    -RandomOps $RandomOps `
    -AppendOps $AppendOps `
    -BigDirFiles $BigDirFiles `
    -EnforceBudget

Convert-TsvToMarkdown -TsvPath $resolvedResults -MarkdownPath $resolvedMarkdown

$historyPath = $null
if (-not $NoPerfHistory) {
    $historyPath = Publish-CoreFsPerfHistory -SourcePath $resolvedResults -Label $HistoryLabel -HistoryDir $HistoryDir
}

Write-Host ""
Write-Host "Results TSV: $resolvedResults"
Write-Host "Markdown log: $resolvedMarkdown"
if ($historyPath) {
    Write-Host "Perf history: $historyPath"
}
