# Installs the porthole CLI from the latest GitHub release.
#
#   irm https://raw.githubusercontent.com/IZ737/Porthole/main/install.ps1 | iex
#
# Settings, all optional, read from the environment:
#
#   PORTHOLE_VERSION      a release such as 1.0.0 (default: the latest)
#   PORTHOLE_INSTALL_DIR  where to put porthole.exe
#                         (default: %LOCALAPPDATA%\Programs\porthole)
#   PORTHOLE_BASE_URL     where the releases live, for mirrors
#                         (default: https://github.com/IZ737/Porthole/releases)
#   PORTHOLE_NO_MODIFY_PATH=1  leave the user PATH alone
#
#   $env:PORTHOLE_VERSION = '1.0.0'; irm .../install.ps1 | iex
#
# The binary is checked against the release's SHA256SUMS before it is
# installed. Works in Windows PowerShell 5.1 and PowerShell 7.

# Everything runs inside one script block, called on the last line, so a
# download cut off halfway through runs nothing, and nothing leaks into the
# caller's session through iex.
& {
    $ErrorActionPreference = 'Stop'
    # Invoke-WebRequest's progress bar makes downloads many times slower in
    # Windows PowerShell 5.1.
    $ProgressPreference = 'SilentlyContinue'

    function Say([string]$msg) { [Console]::Error.WriteLine("porthole-install: $msg") }
    function Fail([string]$msg) { throw "porthole-install: error: $msg" }

    # Windows PowerShell 5.1 can default to TLS 1.0, which GitHub refuses.
    try {
        [Net.ServicePointManager]::SecurityProtocol = `
            [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    } catch {}

    $baseUrl = if ($env:PORTHOLE_BASE_URL) { $env:PORTHOLE_BASE_URL.TrimEnd('/') }
               else { 'https://github.com/IZ737/Porthole/releases' }
    $installDir = if ($env:PORTHOLE_INSTALL_DIR) { $env:PORTHOLE_INSTALL_DIR }
                  else { Join-Path $env:LOCALAPPDATA 'Programs\porthole' }
    $version = if ($env:PORTHOLE_VERSION) { $env:PORTHOLE_VERSION.TrimStart('v') } else { '' }

    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        Fail 'this script is for Windows. On macOS and Linux run:
  curl -fsSL https://raw.githubusercontent.com/IZ737/Porthole/main/install.sh | sh'
    }

    # Releases have an x64 build only. Windows 11 on Arm runs it under
    # emulation; 32-bit Windows cannot.
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    switch ($arch) {
        'AMD64' { }
        'ARM64' { Say 'note: installing the x64 build, which runs under emulation on Arm' }
        default { Fail "there is no prebuilt porthole for $arch. Build it from source instead: cargo install --git https://github.com/IZ737/Porthole porthole-cli" }
    }
    $target = 'x86_64-pc-windows-msvc'

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("porthole-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $sumsUrl = if ($version) { "$baseUrl/download/v$version/SHA256SUMS" }
                   else { "$baseUrl/latest/download/SHA256SUMS" }
        Say "fetching $sumsUrl"
        $sumsPath = Join-Path $tmp 'SHA256SUMS'
        try {
            Invoke-WebRequest -Uri $sumsUrl -OutFile $sumsPath -UseBasicParsing
        } catch {
            $which = if ($version) { " v$version" } else { '' }
            Fail "could not download SHA256SUMS. Is there a release$which? ($($_.Exception.Message))"
        }

        # Lines look like "<sha256>  porthole-1.0.0-<target>.exe". Taking the
        # asset name from SHA256SUMS means "latest" is resolved once, and the
        # binary comes from the same release as its checksum.
        $pattern = '^([0-9a-fA-F]{64})[ *]+(porthole-(\S+)-' + [regex]::Escape($target) + '\.exe)$'
        $match = $null
        foreach ($line in Get-Content -LiteralPath $sumsPath) {
            $m = [regex]::Match($line.Trim(), $pattern)
            if ($m.Success) { $match = $m; break }
        }
        if (-not $match) {
            Fail "the release has no build for $target. Build it from source instead: cargo install --git https://github.com/IZ737/Porthole porthole-cli"
        }
        $expected = $match.Groups[1].Value.ToLowerInvariant()
        $asset = $match.Groups[2].Value
        $assetVersion = $match.Groups[3].Value

        $url = "$baseUrl/download/v$assetVersion/$asset"
        Say "downloading porthole $assetVersion for $target"
        $exePath = Join-Path $tmp $asset
        try {
            Invoke-WebRequest -Uri $url -OutFile $exePath -UseBasicParsing
        } catch {
            Fail "could not download $url ($($_.Exception.Message))"
        }

        $actual = (Get-FileHash -LiteralPath $exePath -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) {
            Fail "checksum mismatch for $asset
  expected $expected
  got      $actual
Nothing was installed."
        }

        New-Item -ItemType Directory -Force -Path $installDir | Out-Null
        $dest = Join-Path $installDir 'porthole.exe'
        # Windows will not overwrite a running exe, but it will rename one.
        # Move the old copy aside so an open porthole does not block the update.
        if (Test-Path -LiteralPath $dest) {
            $old = "$dest.old"
            Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
            try { Move-Item -LiteralPath $dest -Destination $old -Force } catch {
                Fail "could not replace $dest ($($_.Exception.Message))"
            }
        }
        Move-Item -LiteralPath $exePath -Destination $dest -Force
        Remove-Item -LiteralPath "$dest.old" -Force -ErrorAction SilentlyContinue
        Say "installed $dest"
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    # Put the install folder on the user PATH (no admin needed), and on this
    # session's PATH so porthole works straight away.
    $onPath = ($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $installDir.TrimEnd('\') }
    if (-not $onPath) {
        if ($env:PORTHOLE_NO_MODIFY_PATH -eq '1') {
            Say "$installDir is not on your PATH; add it to run porthole by name"
        } else {
            # Edit the registry value directly so entries such as
            # %USERPROFILE%\bin stay unexpanded.
            $key = (Get-Item -Path 'HKCU:\').OpenSubKey('Environment', $true)
            try {
                $userPath = [string]$key.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
                $entries = @($userPath -split ';' | Where-Object { $_ })
                if (-not ($entries | Where-Object { $_.TrimEnd('\') -ieq $installDir.TrimEnd('\') })) {
                    $key.SetValue('Path', (($entries + $installDir) -join ';'), 'ExpandString')
                    # Tell Explorer the environment changed, so new terminals
                    # see it. Setting any user variable broadcasts that.
                    [Environment]::SetEnvironmentVariable('PORTHOLE_INSTALL_PING', '1', 'User')
                    [Environment]::SetEnvironmentVariable('PORTHOLE_INSTALL_PING', $null, 'User')
                    Say "added $installDir to your user PATH (new terminals will see it)"
                }
            } finally {
                $key.Close()
            }
            $env:Path = "$env:Path;$installDir"
        }
    }

    Say "run 'porthole --help' to get started"
}
