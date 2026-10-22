# Installs Rift for the current user. Project setup happens only when they run rift.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

function Fetch-RiftFile([string]$Url, [string]$Destination) {
    $uri = $null
    if (-not [Uri]::TryCreate($Url, [UriKind]::Absolute, [ref]$uri) -or $uri.Scheme -ne 'https' -or $uri.Host -ne 'storage.googleapis.com' -or $uri.UserInfo -or $uri.Fragment) {
        throw 'Use the Google Cloud download link from your invitation.'
    }
    try { Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Destination -TimeoutSec 120 }
    catch { throw 'Download unavailable or invitation expired. Request a fresh link.' }
}
function Assert-RiftRegular([string]$Path) {
    if (Test-Path -LiteralPath $Path) {
        $item = Get-Item -LiteralPath $Path -Force
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $item.PSIsContainer) {
            throw "Refusing linked or non-file installation path: $Path"
        }
    }
}

$bin = $env:RIFT_BIN_DIR
if (-not $bin) { $bin = Join-Path $env:LOCALAPPDATA 'Rift\bin' }
if (-not [IO.Path]::IsPathRooted($bin)) { throw 'RIFT_BIN_DIR must be absolute.' }
if ((Test-Path -LiteralPath $bin) -and ((Get-Item -LiteralPath $bin -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw 'Refusing a linked installation directory.'
}
[IO.Directory]::CreateDirectory($bin) | Out-Null
$lockPath = Join-Path $bin '.rift-install.lock'
$lock = [IO.File]::Open($lockPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
$stage = Join-Path $bin ('.rift-install.' + [Guid]::NewGuid().ToString('N'))
$binary = Join-Path $bin 'rift.exe'
$receipt = Join-Path $bin 'rift.install-receipt'
$previous = $null
$hadBinary = Test-Path -LiteralPath $binary
$receiptHash = $null
$utf8 = New-Object Text.UTF8Encoding($false)
try {
    [IO.Directory]::CreateDirectory($stage) | Out-Null
    Assert-RiftRegular $binary
    Assert-RiftRegular $receipt
    if (Test-Path -LiteralPath $binary) {
        if (-not (Test-Path -LiteralPath $receipt)) { throw 'Existing rift.exe has no installer receipt; it was preserved.' }
        $receiptHash = (Get-FileHash -LiteralPath $receipt -Algorithm SHA256).Hash
        $record = [IO.File]::ReadAllLines($receipt)
        if ($record.Length -lt 3 -or $record[0] -ne 'rift-install-v1') { throw 'Unrecognized installation receipt.' }
        if ((Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant() -ne $record[1]) {
            throw 'Installed binary differs from its receipt; refusing replacement.'
        }
    } elseif (Test-Path -LiteralPath $receipt) { throw 'An orphaned receipt needs inspection before reinstalling.' }
    elseif ($env:RIFT_UPDATE -eq '1') { throw 'The managed executable is missing; reinstall explicitly.' }

    if (-not $env:RIFT_DOWNLOAD_MANIFEST) { throw 'A fresh invitation link is required. Use the install command from your invitation email.' }
    $manifestPath = Join-Path $stage 'manifest'
    Fetch-RiftFile $env:RIFT_DOWNLOAD_MANIFEST $manifestPath
    if ((Get-Item -LiteralPath $manifestPath).Length -gt 1048576) { throw 'Invitation manifest is too large.' }
    $manifestLines = [IO.File]::ReadAllLines($manifestPath)
    if ($manifestLines.Length -lt 2) { throw 'Invalid invitation.' }
    $header = $manifestLines[0] -split "`t"
    if ($header.Length -ne 2 -or $header[0] -ne 'rift-download-v1') { throw 'Invalid invitation.' }
    $version = $header[1]
    if ($version -notmatch '^v1\.0\.0-alpha(\.[0-9]+)?$') { throw 'Invalid or incompatible alpha invitation.' }
    $rows = @($manifestLines | Select-Object -Skip 1 | Where-Object { ($_ -split "`t")[0] -eq 'x86_64-pc-windows-msvc' })
    if ($rows.Length -ne 1) { throw 'Invitation has no unique Windows download.' }
    $fields = $rows[0] -split "`t"
    if ($fields.Length -ne 3 -or $fields[2] -notmatch '^[0-9a-f]{64}$') { throw 'Invalid archive checksum in invitation.' }
    $archiveUrl = $fields[1]
    $expected = $fields[2]
    if ((Test-Path -LiteralPath $binary) -and $record[2] -eq $version) {
        Write-Output "Rift $version is already installed."
        return
    }
    # The x64 build also runs on Windows ARM64 through Windows' x64 emulation.
    if (-not [Environment]::Is64BitOperatingSystem) { throw 'This alpha requires 64-bit Windows.' }
    $archive = Join-Path $stage 'archive.zip'
    Fetch-RiftFile $archiveUrl $archive
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {
        throw 'Archive checksum mismatch; existing installation was preserved.'
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        if ($zip.Entries.Count -ne 1 -or $zip.Entries[0].FullName -ne 'rift.exe') { throw 'Unexpected archive contents.' }
        [IO.Compression.ZipFileExtensions]::ExtractToFile($zip.Entries[0], (Join-Path $stage 'rift.exe'))
    } finally { $zip.Dispose() }
    $stagedBinary = Join-Path $stage 'rift.exe'
    if ((& $stagedBinary --version) -ne "rift $($version.Substring(1))") { throw 'Downloaded executable reports a different version.' }
    $hash = (Get-FileHash -LiteralPath $stagedBinary -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText((Join-Path $stage 'receipt'), "rift-install-v1`n$hash`n$version`n", $utf8)
    Assert-RiftRegular $binary
    Assert-RiftRegular $receipt
    if ($hadBinary) {
        if (-not (Test-Path -LiteralPath $binary) -or -not (Test-Path -LiteralPath $receipt)) { throw 'Installation disappeared while downloading.' }
        if ((Get-FileHash -LiteralPath $receipt -Algorithm SHA256).Hash -ne $receiptHash -or (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant() -ne $record[1]) {
            throw 'Installation changed while downloading; existing files were preserved.'
        }
    } elseif ((Test-Path -LiteralPath $binary) -or (Test-Path -LiteralPath $receipt)) { throw 'Installation paths appeared while downloading; they were preserved.' }
    if (Test-Path -LiteralPath $binary) {
        $previous = Join-Path $bin ('.rift-previous.' + [Guid]::NewGuid().ToString('N') + '.exe')
        [IO.File]::Move($binary, $previous)
    }
    try {
        [IO.File]::Move($stagedBinary, $binary)
        if (Test-Path -LiteralPath $receipt) { [IO.File]::Replace((Join-Path $stage 'receipt'), $receipt, $null) }
        else { [IO.File]::Move((Join-Path $stage 'receipt'), $receipt) }
    } catch {
        if (Test-Path -LiteralPath $binary) { [IO.File]::Delete($binary) }
        if ($previous) { [IO.File]::Move($previous, $binary); $previous = $null }
        throw
    }
    if ($previous) {
        try { [IO.File]::Delete($previous); $previous = $null }
        catch {
            if (-not $env:RIFT_UPDATE_PID) {
                Write-Warning "Previous executable is still running. After it exits, remove: $previous"
            } else {
                $cleanup = @'
$ErrorActionPreference = 'Stop'
try { $parent = [Diagnostics.Process]::GetProcessById([int]$env:RIFT_UPDATE_PID) }
catch [ArgumentException] { $parent = $null }
if ($parent -and -not $parent.WaitForExit(30000)) { exit 1 }
$item = Get-Item -LiteralPath $env:RIFT_PREVIOUS_BINARY -Force
if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { exit 1 }
if ((Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant() -ne $env:RIFT_UPDATE_SHA) { exit 1 }
[IO.File]::Delete($item.FullName)
'@
                $info = New-Object Diagnostics.ProcessStartInfo
                $info.FileName = 'powershell.exe'
                $info.Arguments = '-NoProfile -NonInteractive -EncodedCommand ' + [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($cleanup))
                $info.UseShellExecute = $false
                $info.CreateNoWindow = $true
                $info.EnvironmentVariables['RIFT_PREVIOUS_BINARY'] = $previous
                [Diagnostics.Process]::Start($info).Dispose()
            }
        }
    }
    Write-Output "Installed Rift $version at $binary"
    if (($env:PATH -split ';') -notcontains $bin) {
        Write-Output "Add $bin to your user PATH, then open a terminal and run rift inside your project."
    } else { Write-Output 'Run rift inside your project to choose harnesses and permissions.' }
} finally {
    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
    $lock.Dispose()
    [IO.File]::Delete($lockPath)
}
