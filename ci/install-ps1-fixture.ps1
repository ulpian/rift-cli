# Installs the workspace's debug rift.exe through install.ps1 from a local
# fixture manifest, the Windows counterpart of `xtask release fixture-fetch`
# serving install.sh. It runs under Windows PowerShell 5.1, as `irm | iex` and
# `rift update` do, and proves three things: a lock a closed window left behind
# is reclaimed, a live holder's lock is refused with guidance, and an oversized
# manifest is refused by the download cap. Nothing touches the network.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = Split-Path -Parent $PSScriptRoot
$installer = Join-Path $repo 'install.ps1'
$binary = Join-Path $repo 'target\debug\rift.exe'
if (-not (Test-Path -LiteralPath $binary)) { throw "missing $binary; run cargo test first" }
$root = Join-Path ([IO.Path]::GetTempPath()) ('rift-ps1-fixture-' + [Guid]::NewGuid().ToString('N'))
$assets = Join-Path $root 'assets'
$staging = Join-Path $root 'staging'
$bin = Join-Path $root 'bin'
$lock = Join-Path $bin '.rift-install.lock'
New-Item -ItemType Directory -Path $assets, $staging, $bin | Out-Null

function Write-Manifest([string]$Version, [string]$Hash) {
    $url = 'https://storage.googleapis.com/rift-install-preview/rift-x86_64-pc-windows-msvc.zip'
    [IO.File]::WriteAllText((Join-Path $assets 'downloads.tsv'), "rift-download-v1`t$Version`nx86_64-pc-windows-msvc`t$url`t$Hash`n")
}
function Invoke-Installer {
    $command = "Invoke-Expression ([IO.File]::ReadAllText('$installer'))"
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { $text = & powershell.exe -NoProfile -NonInteractive -Command $command 2>&1 | Out-String }
    finally { $ErrorActionPreference = $previous }
    [pscustomobject]@{ Code = $LASTEXITCODE; Text = $text }
}
function Assert-That([bool]$Condition, [string]$What, $Run) {
    if ($Condition) { return }
    if ($Run) { Write-Output $Run.Text }
    throw "install.ps1 fixture: $What"
}
function Write-Lock([int]$Owner) {
    [IO.File]::WriteAllText($lock, "$Owner $([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())`n")
}

try {
    $version = ((& $binary --version) | Out-String).Trim() -replace '^rift ', 'v'
    Copy-Item -LiteralPath $binary -Destination (Join-Path $staging 'rift.exe')
    $archive = Join-Path $assets 'rift-x86_64-pc-windows-msvc.zip'
    Compress-Archive -Path (Join-Path $staging 'rift.exe') -DestinationPath $archive
    Write-Manifest $version (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    $env:RIFT_INSTALL_FIXTURE_DIR = $assets
    $env:RIFT_DOWNLOAD_MANIFEST = 'https://storage.googleapis.com/rift-install-preview/downloads.tsv'
    $env:RIFT_BIN_DIR = $bin
    $env:RIFT_NONINTERACTIVE = '1'
    $env:RIFT_NO_MODIFY_PATH = '1'
    Remove-Item Env:RIFT_DOWNLOAD_BUCKET, Env:RIFT_UPDATE -ErrorAction SilentlyContinue

    # A lock whose holder has exited, as a closed window leaves it.
    $gone = Start-Process -FilePath cmd.exe -ArgumentList '/c', 'exit' -PassThru -WindowStyle Hidden
    $gone.WaitForExit()
    Write-Lock $gone.Id
    $run = Invoke-Installer
    Assert-That ($run.Code -eq 0 -and $run.Text -match 'Installed Rift') 'a stale lock was not reclaimed' $run
    Assert-That (Test-Path -LiteralPath (Join-Path $bin 'rift.exe')) 'rift.exe was not installed' $run
    Assert-That (Test-Path -LiteralPath (Join-Path $bin 'rift.install-receipt')) 'no receipt was written' $run
    Assert-That (-not (Test-Path -LiteralPath $lock)) 'the lock outlived the installer' $run

    # A lock this live process holds is refused with the shared guidance and kept.
    Write-Lock $PID
    $run = Invoke-Installer
    Assert-That ($run.Code -ne 0 -and $run.Text -match 'Another Rift install') 'a live lock was not refused with guidance' $run
    Assert-That (Test-Path -LiteralPath $lock) 'a live lock was removed' $run
    Remove-Item -LiteralPath $lock

    # A manifest over the 1 MiB cap stops in the capped copy.
    [IO.File]::WriteAllBytes((Join-Path $assets 'downloads.tsv'), (New-Object byte[] (1048576 + 1)))
    $run = Invoke-Installer
    Assert-That ($run.Code -ne 0 -and $run.Text -match 'larger than this') 'an oversized manifest was not refused' $run
    Write-Output 'install.ps1 fixture: stale-lock reclaim, live-lock refusal and download cap verified.'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
