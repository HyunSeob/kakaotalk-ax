$ErrorActionPreference = "Stop"

$Repo = if ($env:KAX_REPO) { $env:KAX_REPO } else { "hyunseob/kakaotalk-ax" }
$Version = if ($env:KAX_VERSION) { $env:KAX_VERSION } else { "latest" }
$InstallDir = if ($env:KAX_INSTALL_DIR) { $env:KAX_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "kakaotalk-ax\bin" }
$Asset = "kax-windows-x86_64.zip"

$BaseUrl = if ($Version -eq "latest") {
    "https://github.com/$Repo/releases/latest/download"
} else {
    "https://github.com/$Repo/releases/download/$Version"
}

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("kax-install-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $TmpDir -Force | Out-Null

try {
    $ZipPath = Join-Path $TmpDir $Asset
    $ChecksumsPath = Join-Path $TmpDir "checksums.txt"

    Write-Host "Downloading $Asset and checksums.txt from $BaseUrl..."
    Invoke-WebRequest -Uri "$BaseUrl/$Asset" -OutFile $ZipPath -UseBasicParsing
    Invoke-WebRequest -Uri "$BaseUrl/checksums.txt" -OutFile $ChecksumsPath -UseBasicParsing

    $Expected = $null
    foreach ($Line in Get-Content -Path $ChecksumsPath) {
        if ($Line -match '^\s*([0-9a-fA-F]{64})\s+\*?(.+?)\s*$') {
            if ($Matches[2] -eq $Asset) {
                $Expected = $Matches[1].ToLowerInvariant()
                break
            }
        }
    }
    if (-not $Expected) {
        throw "Checksum entry for $Asset not found in checksums.txt"
    }

    $Actual = (Get-FileHash -Path $ZipPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($Actual -ne $Expected) {
        throw "SHA-256 mismatch for $Asset (expected $Expected, actual $Actual)"
    }

    $ExtractDir = Join-Path $TmpDir "extract"
    Expand-Archive -Path $ZipPath -DestinationPath $ExtractDir -Force

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item -Path (Join-Path $ExtractDir "kax.exe") -Destination (Join-Path $InstallDir "kax.exe") -Force

    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $Parts = if ($UserPath) { $UserPath -split ';' } else { @() }
    if ($Parts -notcontains $InstallDir) {
        $NewPath = if ($UserPath) { "$InstallDir;$UserPath" } else { $InstallDir }
        [Environment]::SetEnvironmentVariable("Path", $NewPath, "User")
        Write-Host "Added $InstallDir to your user PATH."
    }

    $CurrentParts = @($env:Path -split ';' | Where-Object { $_ -and $_ -ne $InstallDir })
    $env:Path = (@($InstallDir) + $CurrentParts) -join ';'
    Write-Host "Installed kax.exe to $(Join-Path $InstallDir 'kax.exe') (SHA-256 verified)."
} finally {
    Remove-Item -Path $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
}
