# Tests install.ps1 against a fake release served from localhost: the latest
# release, a pinned version, a bad checksum, a missing build, and the user
# PATH. Runs on Windows (CI). Needs python for the web server.
#
#   pwsh ci/test-install.ps1           # or: powershell -File ci/test-install.ps1

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$work = Join-Path ([IO.Path]::GetTempPath()) ("porthole-install-test-" + [Guid]::NewGuid().ToString('N'))
$target = 'x86_64-pc-windows-msvc'
$shell = (Get-Process -Id $PID).Path   # run install.ps1 in the same PowerShell

function Fail([string]$msg) { throw "FAIL: $msg" }

# A fake release; the "exe" is just bytes, since only the download and the
# checksum are under test.
function Make-Release([string]$dir, [string]$ver) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $exe = "porthole-$ver-$target.exe"
    Set-Content -LiteralPath (Join-Path $dir $exe) -Value "fake porthole $ver" -NoNewline
    Set-Content -LiteralPath (Join-Path $dir "porthole-$ver-x86_64-unknown-linux-gnu") -Value 'other build' -NoNewline
    $lines = Get-ChildItem -LiteralPath $dir -Filter 'porthole-*' | Sort-Object Name | ForEach-Object {
        "$((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant())  $($_.Name)"
    }
    # LF endings and no BOM, like the release's SHA256SUMS.
    [IO.File]::WriteAllText((Join-Path $dir 'SHA256SUMS'), (($lines -join "`n") + "`n"))
}

$srv = Join-Path $work 'srv\releases'
Make-Release (Join-Path $srv 'download\v1.2.3') '1.2.3'
Make-Release (Join-Path $srv 'download\v1.1.0') '1.1.0'
New-Item -ItemType Directory -Force -Path (Join-Path $srv 'latest\download') | Out-Null
Copy-Item (Join-Path $srv 'download\v1.2.3\SHA256SUMS') (Join-Path $srv 'latest\download\SHA256SUMS')
Make-Release (Join-Path $srv 'download\v0.9.0') '0.9.0'
Add-Content -LiteralPath (Join-Path $srv "download\v0.9.0\porthole-0.9.0-$target.exe") -Value 'tampered'
New-Item -ItemType Directory -Force -Path (Join-Path $srv 'download\v0.8.0') | Out-Null
$noWin = (Get-Content (Join-Path $srv 'download\v1.2.3\SHA256SUMS')) | Where-Object { $_ -notmatch [regex]::Escape($target) }
[IO.File]::WriteAllText((Join-Path $srv 'download\v0.8.0\SHA256SUMS'), (($noWin -join "`n") + "`n"))

$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$listener.Start(); $port = $listener.LocalEndpoint.Port; $listener.Stop()
$server = Start-Process python -ArgumentList '-m', 'http.server', $port, '--bind', '127.0.0.1', '-d', (Join-Path $work 'srv') `
    -PassThru -WindowStyle Hidden
$base = "http://127.0.0.1:$port/releases"

# The test writes to the real user PATH; put it back afterwards.
$envKey = (Get-Item -Path 'HKCU:\').OpenSubKey('Environment', $true)
$savedPath = $envKey.GetValue('Path', $null, 'DoNotExpandEnvironmentNames')

function Run-Install([string]$dir, [string]$ver = '') {
    $env:PORTHOLE_BASE_URL = $base
    $env:PORTHOLE_INSTALL_DIR = $dir
    $env:PORTHOLE_VERSION = $ver
    # Windows PowerShell 5.1 turns a native command's stderr into errors,
    # which 'Stop' would throw on.
    $ErrorActionPreference = 'Continue'
    # Run it the way the README does: piped into Invoke-Expression.
    $out = & $shell -NoProfile -NonInteractive -Command "Get-Content -Raw '$root\install.ps1' | Invoke-Expression" 2>&1 | Out-String
    return @{ Code = $LASTEXITCODE; Out = $out }
}

function Expected-Hash([string]$ver) {
    $line = Get-Content (Join-Path $srv "download\v$ver\SHA256SUMS") | Where-Object { $_ -match [regex]::Escape("$target.exe") }
    return ($line -split '\s+')[0]
}
function Installed-Hash([string]$dir) {
    return (Get-FileHash -LiteralPath (Join-Path $dir 'porthole.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
}

try {
    for ($i = 0; ; $i++) {
        try { Invoke-WebRequest "$base/latest/download/SHA256SUMS" -UseBasicParsing | Out-Null; break }
        catch { if ($i -ge 100) { Fail 'server did not start' }; Start-Sleep -Milliseconds 100 }
    }

    $bin = Join-Path $work 'bin'

    Write-Host '== latest'
    $r = Run-Install $bin
    Write-Host $r.Out
    if ($r.Code -ne 0) { Fail 'latest install failed' }
    if ((Installed-Hash $bin) -ne (Expected-Hash '1.2.3')) { Fail 'latest did not install 1.2.3' }
    $userPath = $envKey.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    if (-not (($userPath -split ';') -contains $bin)) { Fail "user PATH does not have $bin" }

    Write-Host '== pinned version, replacing the existing exe, PATH not added twice'
    $r = Run-Install $bin 'v1.1.0'
    if ($r.Code -ne 0) { Write-Host $r.Out; Fail 'pinned install failed' }
    if ((Installed-Hash $bin) -ne (Expected-Hash '1.1.0')) { Fail 'pinned did not install 1.1.0' }
    if (Test-Path (Join-Path $bin 'porthole.exe.old')) { Fail 'old exe left behind' }
    $userPath = $envKey.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    if ((($userPath -split ';') | Where-Object { $_ -eq $bin }).Count -ne 1) { Fail 'PATH entry added twice' }

    Write-Host '== checksum mismatch'
    $bad = Join-Path $work 'bad'
    $r = Run-Install $bad '0.9.0'
    Write-Host $r.Out
    if ($r.Code -eq 0) { Fail 'bad checksum was accepted' }
    if ($r.Out -notmatch 'checksum mismatch') { Fail 'wrong error for a bad checksum' }
    if (Test-Path (Join-Path $bad 'porthole.exe')) { Fail 'installed despite a bad checksum' }

    Write-Host '== no build for this platform'
    $r = Run-Install (Join-Path $work 'none') '0.8.0'
    Write-Host $r.Out
    if ($r.Code -eq 0) { Fail 'missing build was accepted' }
    if ($r.Out -notmatch "no build for $target") { Fail 'wrong error for a missing build' }

    Write-Host '== release that does not exist'
    $r = Run-Install (Join-Path $work 'none') '9.9.9'
    if ($r.Code -eq 0) { Fail 'missing release was accepted' }
    if ($r.Out -notmatch 'Is there a release v9\.9\.9') { Write-Host $r.Out; Fail 'wrong error for a missing release' }

    Write-Host "install.ps1: all checks passed ($($PSVersionTable.PSEdition) $($PSVersionTable.PSVersion))"
} finally {
    if ($null -eq $savedPath) { $envKey.DeleteValue('Path', $false) }
    else { $envKey.SetValue('Path', $savedPath, 'ExpandString') }
    $envKey.Close()
    Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

# The last install run above failed on purpose; do not let its exit code
# become this script's.
exit 0
