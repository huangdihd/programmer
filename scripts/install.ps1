<#
.SYNOPSIS
    Install programmer from a GitHub Release on Windows.

.DESCRIPTION
    Downloads the prebuilt programmer.exe for this machine's architecture from
    GitHub Releases, installs it into %LOCALAPPDATA%\programmer\bin (or a custom
    directory), and adds that directory to the user PATH.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File install.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File install.ps1 -Version v0.2.0

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File install.ps1 -InstallDir D:\bin
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$Version = "latest",
    [string]$InstallDir = "",
    [switch]$NoPath
)

$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    Write-Host "install.ps1: error: $Message" -ForegroundColor Red
    exit 1
}

# Map the running process architecture to a release asset target.
$arch = [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()
switch ($arch) {
    "X64"   { $target = "x86_64-pc-windows-msvc" }
    "Arm64" { $target = "aarch64-pc-windows-msvc" }
    "X86"   { $target = "i686-pc-windows-msvc" }
    default { Fail "unsupported architecture: $arch" }
}

# PowerShell 5.1 defaults to TLS 1.0; modern .NET already prefers TLS 1.2+.
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
    # Nothing to do — the runtime already uses TLS 1.2+.
}

$base = "https://github.com/huangdihd/programmer/releases"
if ($Version -eq "latest") {
    $url = "$base/latest/download/programmer-$target.zip"
} else {
    $url = "$base/download/$Version/programmer-$target.zip"
}

$zip = Join-Path $env:TEMP "programmer-$target.zip"
Write-Host "Downloading $url"
if ($PSVersionTable.PSVersion.Major -ge 6) {
    Invoke-WebRequest -Uri $url -OutFile $zip
} else {
    Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
}

$tempDir = Join-Path $env:TEMP ("programmer-install-" + [System.Guid]::NewGuid().ToString("N"))
try {
    Expand-Archive -Path $zip -DestinationPath $tempDir -Force
    $exe = Join-Path $tempDir "programmer.exe"
    if (-not (Test-Path -LiteralPath $exe)) {
        Fail "archive does not contain programmer.exe"
    }

    if (-not $InstallDir) {
        $InstallDir = Join-Path $env:LOCALAPPDATA "programmer\bin"
    }
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $dest = Join-Path $InstallDir "programmer.exe"
    Copy-Item -LiteralPath $exe -Destination $dest -Force

    Write-Host "Installed $dest"
    & $dest --version
    if ($LASTEXITCODE -ne 0) {
        Remove-Item -LiteralPath $dest -Force -ErrorAction SilentlyContinue
        Fail "installed binary failed its version check"
    }

    if (-not $NoPath) {
        $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
        if (($userPath -split ";") -notcontains $InstallDir) {
            $newPath = if ([string]::IsNullOrEmpty($userPath)) { $InstallDir } else { "$userPath;$InstallDir" }
            [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
            Write-Host "Added $InstallDir to your user PATH. Open a new terminal to run 'programmer'."
        }
    }
} finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
}
