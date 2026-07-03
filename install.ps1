# limpet installer for Windows: download the latest release binary, verify
# its sha256, install it, add it to the user PATH, and register it with
# Claude Code.
#
#   irm https://raw.githubusercontent.com/KSym04/limpet/main/install.ps1 | iex
#
# Environment:
#   LIMPET_INSTALL_DIR  target directory (default: %LOCALAPPDATA%\Programs\limpet)
#   LIMPET_VERSION      release tag to install (default: latest)
$ErrorActionPreference = 'Stop'

$repo = 'KSym04/limpet'
$installDir = if ($env:LIMPET_INSTALL_DIR) { $env:LIMPET_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\limpet' }
$version = if ($env:LIMPET_VERSION) { $env:LIMPET_VERSION } else { 'latest' }
$asset = 'limpet-x86_64-pc-windows-msvc.exe'

if ($version -eq 'latest') {
    $base = "https://github.com/$repo/releases/latest/download"
} else {
    $base = "https://github.com/$repo/releases/download/$version"
}

$tmp = Join-Path $env:TEMP ("limpet-install-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "limpet downloading $asset ($version)"
    Invoke-WebRequest "$base/$asset" -OutFile (Join-Path $tmp $asset)
    Invoke-WebRequest "$base/$asset.sha256" -OutFile (Join-Path $tmp "$asset.sha256")

    Write-Host 'limpet verifying sha256'
    $actual = (Get-FileHash (Join-Path $tmp $asset) -Algorithm SHA256).Hash.ToLowerInvariant()
    $expected = ((Get-Content (Join-Path $tmp "$asset.sha256") -Raw) -split '\s+')[0].ToLowerInvariant()
    if ($actual -ne $expected) {
        throw 'sha256 mismatch - download corrupted or tampered, aborting'
    }

    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    Copy-Item (Join-Path $tmp $asset) (Join-Path $installDir 'limpet.exe') -Force
    Write-Host "limpet installed $installDir\limpet.exe"

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (($userPath -split ';') -notcontains $installDir) {
        [Environment]::SetEnvironmentVariable('Path', "$userPath;$installDir", 'User')
        $env:Path = "$env:Path;$installDir"
        Write-Host "limpet added $installDir to your user PATH (restart terminals to pick it up)"
    }

    Write-Host 'limpet registering with Claude Code'
    & (Join-Path $installDir 'limpet.exe') install
    Write-Host 'limpet done - restart Claude Code, then type /limpet in any project'
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
