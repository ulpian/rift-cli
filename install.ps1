# Installs Rift for the current user. Project setup happens only when they run rift.
# `irm | iex` evaluates this text in the caller's own session, so everything runs
# inside one child scope: strict mode, the stop-on-error preference, the helper
# functions and every variable end with the installer instead of staying behind
# in the user's PowerShell window. Only PATH is meant to outlive it.
& {
$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 redraws a progress bar per chunk, which slows a download tenfold.
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
# The setting is process-wide, so it is only ever widened. Zero means "follow the
# operating system", which already allows TLS 1.2 and newer: leave that alone.
$tls = [Net.ServicePointManager]::SecurityProtocol
if ([int]$tls -ne 0 -and -not ([int]$tls -band [int][Net.SecurityProtocolType]::Tls12)) {
    [Net.ServicePointManager]::SecurityProtocol = $tls -bor [Net.SecurityProtocolType]::Tls12
}

function Fetch-RiftFile([string]$Url, [string]$Destination) {
    $uri = $null
    if (-not [Uri]::TryCreate($Url, [UriKind]::Absolute, [ref]$uri) -or $uri.Scheme -ne 'https' -or $uri.Host -ne 'storage.googleapis.com' -or $uri.UserInfo -or $uri.Fragment) {
        throw 'Use the Google Cloud download link from your invitation.'
    }
    # The published installer and `rift update` name the release bucket: a link
    # into any other bucket is refused, however valid its checksums look,
    # because the manifest that carries them came from the same place.
    if ($env:RIFT_DOWNLOAD_BUCKET -and -not $uri.AbsolutePath.StartsWith("/$($env:RIFT_DOWNLOAD_BUCKET)/", [StringComparison]::Ordinal)) {
        throw 'This link is not from the Rift release bucket. Use the link from your invitation.'
    }
    try { Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Destination -TimeoutSec 120 }
    catch { throw 'Download unavailable or invitation expired. Request a fresh link.' }
}
# Release tags this installer understands: vMAJOR.MINOR.PATCH-alpha[.N],
# each number one to six digits.
$RiftAlphaTag = '^v([0-9]{1,6})\.([0-9]{1,6})\.([0-9]{1,6})-alpha(?:\.([0-9]{1,6}))?$'
# A fixed-width key ordering alpha tags: major, minor, patch, then N (a plain
# -alpha is N = 0). Call only on a tag that matched $RiftAlphaTag.
function Get-RiftAlphaKey([string]$Version) {
    $m = [regex]::Match($Version, $RiftAlphaTag)
    $n = if ($m.Groups[4].Success) { [int]$m.Groups[4].Value } else { 0 }
    return '{0:D6}{1:D6}{2:D6}{3:D6}' -f [int]$m.Groups[1].Value, [int]$m.Groups[2].Value, [int]$m.Groups[3].Value, $n
}
function Assert-RiftRegular([string]$Path) {
    if (Test-Path -LiteralPath $Path) {
        $item = Get-Item -LiteralPath $Path -Force
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $item.PSIsContainer) {
            throw "Refusing linked or non-file installation path: $Path"
        }
    }
}

function Set-RiftPreferences {
    if ($env:RIFT_UPDATE -eq '1') { return }
    if ($env:RIFT_NONINTERACTIVE -ne '1' -and [Environment]::UserInteractive -and -not [Console]::IsInputRedirected -and -not [Console]::IsOutputRedirected) {
        & $binary install
        if ($LASTEXITCODE -ne 0) { throw 'Binary installed; preferences incomplete. Run rift install to resume.' }
    } else { Write-Output 'Run rift install in a terminal to choose default harnesses.' }
}

# The user PATH is read unexpanded: writing back an expanded value would freeze
# every %VARIABLE% another program stored there. `irm | iex` runs in the caller's
# session, so updating the process PATH makes rift resolve in this window at once.
function Add-RiftToPath([string]$Directory) {
    if ($env:RIFT_NO_MODIFY_PATH -eq '1' -or $Directory.Contains(';')) { return $false }
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    if (-not $key) { return $false }
    try {
        $stored = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $entries = @($stored -split ';' | Where-Object { $_ })
        $wanted = $Directory.TrimEnd('\')
        $listed = @($entries | Where-Object { [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') -ieq $wanted })
        if ($listed.Length -eq 0) {
            $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
            if ($stored) { $kind = $key.GetValueKind('Path') }
            $key.SetValue('Path', ((@($Directory) + $entries) -join ';'), $kind)
            # Setting then clearing a throwaway variable broadcasts the change to open applications.
            $probe = 'RIFT_PATH_' + [Guid]::NewGuid().ToString('N')
            [Environment]::SetEnvironmentVariable($probe, '1', 'User')
            [Environment]::SetEnvironmentVariable($probe, [NullString]::Value, 'User')
        }
    } finally { $key.Close() }
    if (@($env:PATH -split ';' | Where-Object { $_.TrimEnd('\') -ieq $wanted }).Length -eq 0) { $env:PATH = "$Directory;$env:PATH" }
    return $true
}
function Complete-RiftInstall {
    if ($env:RIFT_UPDATE -eq '1') { return }
    $registered = $false
    try { $registered = Add-RiftToPath $bin } catch { $registered = $false }
    if ($registered) {
        Write-Output "$bin is on your user PATH. Terminals that are already open need a restart."
        Write-Output 'Run rift inside your project to choose harnesses and permissions.'
    } else { Write-Output "Add $bin to your user PATH, then open a terminal and run rift inside your project." }
}

if ($env:RIFT_DOWNLOAD_BUCKET -and $env:RIFT_DOWNLOAD_BUCKET -cnotmatch '^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$') { throw 'Invalid release bucket name.' }
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
    Write-Output "  @--o--o  Step 1/3: Download invitation and binary"
    Fetch-RiftFile $env:RIFT_DOWNLOAD_MANIFEST $manifestPath
    if ((Get-Item -LiteralPath $manifestPath).Length -gt 1048576) { throw 'Invitation manifest is too large.' }
    $manifestLines = [IO.File]::ReadAllLines($manifestPath)
    if ($manifestLines.Length -lt 2) { throw 'Invalid invitation.' }
    $header = $manifestLines[0] -split "`t"
    if ($header.Length -ne 2 -or $header[0] -ne 'rift-download-v1') { throw 'Invalid invitation.' }
    $version = $header[1]
    if ($version -notmatch $RiftAlphaTag) { throw 'Invalid or incompatible alpha invitation.' }
    $rows = @($manifestLines | Select-Object -Skip 1 | Where-Object { ($_ -split "`t")[0] -eq 'x86_64-pc-windows-msvc' })
    if ($rows.Length -ne 1) { throw 'Invitation has no unique Windows download.' }
    $fields = $rows[0] -split "`t"
    if ($fields.Length -ne 3 -or $fields[2] -notmatch '^[0-9a-f]{64}$') { throw 'Invalid archive checksum in invitation.' }
    $archiveUrl = $fields[1]
    $expected = $fields[2]
    if ((Test-Path -LiteralPath $binary) -and $env:RIFT_ALLOW_DOWNGRADE -ne '1' -and $record[2] -match $RiftAlphaTag -and
        [string]::CompareOrdinal((Get-RiftAlphaKey $record[2]), (Get-RiftAlphaKey $version)) -gt 0) {
        throw "Rift $($record[2]) is newer than $version; nothing was changed. Set RIFT_ALLOW_DOWNGRADE=1 to install the older build."
    }
    if ((Test-Path -LiteralPath $binary) -and $record[2] -eq $version) {
        Write-Output "Rift $version is already installed."
        Set-RiftPreferences
        Complete-RiftInstall
        return
    }
    # The x64 build also runs on Windows ARM64 through Windows' x64 emulation.
    if (-not [Environment]::Is64BitOperatingSystem) { throw 'This alpha requires 64-bit Windows.' }
    $archive = Join-Path $stage 'archive.zip'
    Fetch-RiftFile $archiveUrl $archive
    Write-Output "  *--@--o  Step 2/3: Verify archive and executable"
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
    Write-Output "  *--*--@  Step 3/3: Install verified binary"
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
    Write-Output "  *--*--*  Binary installation complete"
    Write-Output "Installed Rift $version at $binary"
    Set-RiftPreferences
    Complete-RiftInstall
} finally {
    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
    $lock.Dispose()
    [IO.File]::Delete($lockPath)
}
}
