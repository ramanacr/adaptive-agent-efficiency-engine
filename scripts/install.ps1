<#
.SYNOPSIS
  Install or update AAES on Windows from a GitHub Release. No admin rights needed.

.DESCRIPTION
  Downloads the release archive for this machine (x64 or arm64), checks it against the
  release's SHA256SUMS, and installs aaes.exe into a per-user folder. If the GitHub CLI
  (`gh`) is available, it also verifies the Sigstore build provenance.
  A running `aaes serve` is stopped first, so the binary can be replaced.
  The install folder is added to the user PATH unless -NoPath is given.
  Then run `aaes init` to add the Claude Code hooks.

.EXAMPLE
  & ([scriptblock]::Create((irm https://raw.githubusercontent.com/ramanacr/adaptive-agent-efficiency-engine/main/scripts/install.ps1)))

.EXAMPLE
  .\install.ps1 -Version 0.1.0 -InstallDir D:\Tools\aaes
#>
param(
    # Release version without the leading "v". Default: the latest published release.
    [string]$Version = '',
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\aaes'),
    # Install from a local folder that holds the archive and SHA256SUMS (for testing).
    [string]$Source = '',
    [switch]$NoPath
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$repo = 'ramanacr/adaptive-agent-efficiency-engine'

$arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
$target = switch ($arch) {
    'X64' { 'x86_64-pc-windows-msvc' }
    'Arm64' { 'aarch64-pc-windows-msvc' }
    default { throw "Unsupported architecture: $arch" }
}

if (-not $Version) {
    if ($Source) {
        $f = Get-ChildItem $Source -Filter "aaes-v*-$target.zip" | Select-Object -First 1
        if (-not $f) { throw "No aaes-v*-$target.zip in $Source" }
        $Version = $f.Name -replace '^aaes-v(.+?)-.*$', '$1'
    } else {
        $rel = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest" -Headers @{ 'User-Agent' = 'aaes-installer' }
        $Version = $rel.tag_name.TrimStart('v')
    }
}
$archive = "aaes-v$Version-$target.zip"
$tmp = Join-Path ([IO.Path]::GetTempPath()) "aaes-install-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory $tmp | Out-Null
try {
    foreach ($name in $archive, 'SHA256SUMS') {
        $dest = Join-Path $tmp $name
        if ($Source) {
            Copy-Item (Join-Path $Source $name) $dest
        } else {
            Invoke-WebRequest "https://github.com/$repo/releases/download/v$Version/$name" -OutFile $dest -UseBasicParsing
        }
    }
    $zip = Join-Path $tmp $archive
    $line = Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { $_ -match "\s\*?$([regex]::Escape($archive))$" } | Select-Object -First 1
    if (-not $line) { throw "$archive is not listed in SHA256SUMS" }
    $expected = ($line -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($expected -ne $actual) { throw "Checksum mismatch for $archive (expected $expected, got $actual)" }
    Write-Host "Checksum OK: $archive"

    if (-not $Source -and (Get-Command gh -ErrorAction SilentlyContinue)) {
        & gh auth status 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) {
            Write-Host 'Skipped the build provenance check: gh is not signed in (gh auth login).'
        } else {
            & gh attestation verify $zip --repo $repo 2>&1 | Out-Null
            if ($LASTEXITCODE -eq 0) { Write-Host 'Build provenance verified (Sigstore).' }
            else { throw "Build provenance check failed for $archive (gh attestation verify)." }
        }
    }

    $exe = Join-Path $InstallDir 'aaes.exe'
    if (Test-Path $exe) {
        & $exe serve --stop 2>&1 | Out-Null
        Start-Sleep -Milliseconds 800
    }
    New-Item -ItemType Directory -Force $InstallDir | Out-Null
    Expand-Archive $zip -DestinationPath $tmp -Force
    Copy-Item (Join-Path $tmp 'aaes.exe') $exe -Force
    Write-Host "Installed $(& $exe --version) to $exe"

    if (-not $NoPath) {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $parts = @($userPath -split ';' | Where-Object { $_ })
        if ($parts -notcontains $InstallDir) {
            [Environment]::SetEnvironmentVariable('Path', (($parts + $InstallDir) -join ';'), 'User')
            Write-Host "Added $InstallDir to your user PATH. Open a new terminal to use 'aaes'."
        }
    }
    Write-Host "Next: run 'aaes init' to add the Claude Code hooks."
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
