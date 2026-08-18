param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+([\-+][0-9A-Za-z.-]+)?$')]
    [string]$Version,
    [string]$ReleaseNotes = "",
    [ValidateSet('auto', 'cuda', 'vulkan', 'all')]
    [string]$RuntimeBackend = 'auto',
    [ValidateSet('nsis', 'msi', 'all')]
    [string]$BundleTarget = 'all'
)

$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($env:TAURI_SIGNING_PRIVATE_KEY)) {
    throw 'TAURI_SIGNING_PRIVATE_KEY must contain the updater private key.'
}
if ([string]::IsNullOrWhiteSpace($env:TAURI_UPDATER_PUBKEY)) {
    throw 'TAURI_UPDATER_PUBKEY must contain the matching public key.'
}
# A password is only needed for an encrypted signing key. Windows cannot hold an
# empty environment variable at all - assigning '' deletes it - so demanding this
# variable made a release with an unencrypted key impossible to build. Tauri asks
# for a password only when the key is actually encrypted, and the build below
# fails loudly if signing does not happen.

$repoRoot = Split-Path -Parent $PSScriptRoot
$desktopRoot = Join-Path $repoRoot 'desktop'
$tauriRoot = Join-Path $desktopRoot 'src-tauri'
$templatePath = Join-Path $tauriRoot 'tauri.release.conf.template.json'
$releaseConfigPath = Join-Path $tauriRoot 'tauri.release.conf.json'
$releaseDir = Join-Path $repoRoot "release\$Version"
$engineResourceRoot = Join-Path $tauriRoot 'resources\minimaxmusic-cpp'

Push-Location $repoRoot
try {
    # Release output must be built from tested native code.  The installer
    # deliberately contains only executables and UI assets; model weights stay
    # in the first-run model manager and are never downloaded at launch.
    # The studio is a single executable: the service is compiled into the
    # desktop binary, so there is no second program to build, ship or launch.
    # A native program writing to stderr is not a failure, but with the error
    # preference on Stop and the output redirected to a log, PowerShell turns
    # every cargo warning into a terminating error. Exit codes are the truth
    # here, so they are checked directly.
    $ErrorActionPreference = 'Continue'
    cargo test --workspace
    if ($LASTEXITCODE -ne 0) { throw "cargo test failed with exit code $LASTEXITCODE" }
    & (Join-Path $PSScriptRoot 'build-minimax-runtime.ps1') -OutputDirectory $engineResourceRoot -RuntimeBackend $RuntimeBackend -CudaArchitecture universal
    if ($LASTEXITCODE -ne 0) { throw "the engine runtime build failed with exit code $LASTEXITCODE" }

    $config = Get-Content -Raw $templatePath
    $config = $config.Replace('__TAURI_UPDATER_PUBKEY__', $env:TAURI_UPDATER_PUBKEY)
    $config = $config.Replace('__RELEASE_VERSION__', $Version)
    Set-Content -Path $releaseConfigPath -Value $config -NoNewline

    Push-Location $desktopRoot
    try {
        if ($BundleTarget -eq 'all') {
            npm exec tauri build -- --config $releaseConfigPath
        }
        else {
            npm exec tauri build -- --config $releaseConfigPath --bundles $BundleTarget
        }
        if ($LASTEXITCODE -ne 0) { throw "tauri build failed with exit code $LASTEXITCODE" }
    }
    finally {
        Pop-Location
    }

    New-Item -ItemType Directory -Force -Path $releaseDir | Out-Null
    $bundleRoot = Join-Path $tauriRoot 'target\release\bundle'
    Get-ChildItem -Recurse -File $bundleRoot -Include '*.exe','*.msi','*.sig' |
        Copy-Item -Destination $releaseDir -Force

    $portableRoot = Join-Path $releaseDir "MiniMax-Music3-Studio-$Version-portable"
    New-Item -ItemType Directory -Force -Path $portableRoot | Out-Null
    Copy-Item (Join-Path $tauriRoot 'target\release\minimax-music3-studio-desktop.exe') (Join-Path $portableRoot 'MiniMax-Music3-Studio.exe') -Force
    Copy-Item $engineResourceRoot (Join-Path $portableRoot 'resources\minimaxmusic-cpp') -Recurse -Force
    New-Item -ItemType File -Path (Join-Path $portableRoot 'portable.flag') -Force | Out-Null
    Compress-Archive -Path "$portableRoot\*" -DestinationPath (Join-Path $releaseDir "MiniMax-Music3-Studio-$Version-portable.zip") -Force

    # Tauri signs updater artifacts, but intentionally does not invent a
    # hosting-specific latest.json.  Build it from the actual NSIS artifact so
    # the in-app updater always receives a real URL and signature.
    $nsisInstaller = Get-ChildItem -Recurse -File (Join-Path $bundleRoot 'nsis') -Filter '*-setup.exe' |
        Select-Object -First 1
    if (-not $nsisInstaller) {
        throw 'NSIS setup executable is missing. Build with -BundleTarget nsis or all; updater releases require NSIS.'
    }
    $signaturePath = "$($nsisInstaller.FullName).sig"
    if (-not (Test-Path $signaturePath)) {
        throw "Signed updater artifact is missing: $signaturePath"
    }
    $signature = (Get-Content -Raw $signaturePath).Trim()
    if ([string]::IsNullOrWhiteSpace($signature)) {
        throw "Updater signature is empty: $signaturePath"
    }
    $assetName = $nsisInstaller.Name -replace ' ', '.'
    if ($assetName -ne $nsisInstaller.Name) {
        Copy-Item $nsisInstaller.FullName (Join-Path $releaseDir $assetName) -Force
        Copy-Item $signaturePath (Join-Path $releaseDir "$assetName.sig") -Force
    }
    $latest = [ordered]@{
        version = $Version
        notes = $ReleaseNotes
        pub_date = (Get-Date).ToUniversalTime().ToString('o')
        platforms = [ordered]@{
            'windows-x86_64' = [ordered]@{
                signature = $signature
                url = "https://github.com/timoncool/MiniMax-Music3-Studio/releases/download/v$Version/$assetName"
            }
        }
    }
    $latest | ConvertTo-Json -Depth 8 | Set-Content -Path (Join-Path $releaseDir 'latest.json') -Encoding utf8NoBOM
}
finally {
    Remove-Item -LiteralPath $releaseConfigPath -Force -ErrorAction SilentlyContinue
    Pop-Location
}
