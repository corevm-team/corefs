param(
    [ValidateSet("Auto", "Winget", "GitHub")]
    [string]$Method = "Auto",
    [string]$ReleaseTag = "latest",
    [string]$DownloadDir = ".\target\winfsp-installer",
    [switch]$Force,
    [switch]$Quiet,
    [switch]$NoElevate
)

. (Join-Path $PSScriptRoot "corefs-common.ps1")

function Test-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Restart-Elevated {
    $args = @(
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        $PSCommandPath,
        "-Method",
        $Method,
        "-ReleaseTag",
        $ReleaseTag,
        "-DownloadDir",
        $DownloadDir
    )

    if ($Force) {
        $args += "-Force"
    }
    if ($Quiet) {
        $args += "-Quiet"
    }
    if ($NoElevate) {
        $args += "-NoElevate"
    }

    $process = Start-Process -FilePath "powershell.exe" -ArgumentList $args -Verb RunAs -Wait -PassThru
    exit $process.ExitCode
}

function Install-WinFspWithWinget {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        throw "winget wurde nicht gefunden."
    }

    $wingetArgs = @(
        "install",
        "--exact",
        "--id",
        "WinFsp.WinFsp",
        "--accept-package-agreements",
        "--accept-source-agreements"
    )

    if ($Force) {
        $wingetArgs += "--force"
    }
    if ($Quiet) {
        $wingetArgs += "--silent"
    }

    & winget @wingetArgs
    if ($LASTEXITCODE -ne 0) {
        throw "winget install WinFsp.WinFsp failed with exit code $LASTEXITCODE."
    }
}

function Get-WinFspRelease {
    if ($ReleaseTag -eq "latest") {
        return Invoke-RestMethod -Uri "https://api.github.com/repos/winfsp/winfsp/releases/latest" -Headers @{ "User-Agent" = "corefs-installer" }
    }

    return Invoke-RestMethod -Uri "https://api.github.com/repos/winfsp/winfsp/releases/tags/$ReleaseTag" -Headers @{ "User-Agent" = "corefs-installer" }
}

function Install-WinFspFromGitHub {
    $resolvedDownloadDir = [System.IO.Path]::GetFullPath($DownloadDir)
    New-Item -ItemType Directory -Force -Path $resolvedDownloadDir | Out-Null

    $release = Get-WinFspRelease
    $asset = $release.assets |
        Where-Object { $_.name -like "winfsp-*.msi" } |
        Sort-Object name -Descending |
        Select-Object -First 1

    if (-not $asset) {
        throw "Kein WinFSP-MSI im GitHub-Release '$($release.tag_name)' gefunden."
    }

    $msiPath = Join-Path $resolvedDownloadDir $asset.name
    Write-Host "Downloading WinFSP $($release.tag_name): $($asset.browser_download_url)"
    Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $msiPath

    Write-Host "Installing $msiPath"
    $msiArgs = @("/i", $msiPath, "/norestart")
    if ($Quiet) {
        $msiArgs += "/qn"
    }
    else {
        $msiArgs += "/passive"
    }

    $process = Start-Process -FilePath "msiexec.exe" -ArgumentList $msiArgs -Wait -PassThru
    if ($process.ExitCode -ne 0) {
        throw "msiexec failed with exit code $($process.ExitCode)."
    }
}

if ((Test-CoreFsWinFspInstalled) -and -not $Force) {
    Write-Host "WinFSP scheint bereits installiert zu sein."
    return
}

if (-not (Test-Administrator)) {
    if ($NoElevate) {
        throw "WinFSP-Installation benoetigt Administratorrechte. Starte PowerShell als Administrator oder lasse -NoElevate weg."
    }

    Write-Host "Administratorrechte werden fuer die WinFSP-Installation angefordert..."
    Restart-Elevated
}

if ($Method -eq "Winget" -or ($Method -eq "Auto" -and (Get-Command winget -ErrorAction SilentlyContinue))) {
    Install-WinFspWithWinget
}
else {
    Install-WinFspFromGitHub
}

if (Test-CoreFsWinFspInstalled) {
    Write-Host "WinFSP ist installiert und wurde von CoreFS erkannt."
}
else {
    throw "WinFSP-Installation abgeschlossen, aber CoreFS konnte die Runtime nicht erkennen. Eventuell ist ein neues Terminal oder ein Neustart erforderlich."
}
