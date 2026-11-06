param([Parameter(Mandatory)] [string] $Destination)

$ErrorActionPreference = 'Stop'
$version = '5.0.1'
$expectedHash = 'b12a2c4ee8be317422451fc7dcf8ee20a71b5ea7ef9ad73ddd825a25316227a5'
$archiveName = "scrcpy-win64-v$version"
$url = "https://github.com/Genymobile/scrcpy/releases/download/v$version/$archiveName.zip"
$destinationPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Destination)
if (Test-Path -LiteralPath $destinationPath) {
    throw "Destination already exists: $destinationPath. Use a new directory for the bundle."
}
$staging = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $staging | Out-Null
$archive = Join-Path $staging "$archiveName.zip"
Invoke-WebRequest -Uri $url -OutFile $archive
$actualHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
if ($actualHash -ne $expectedHash) {
    throw "SHA-256 mismatch for $archiveName.zip. Bundle not installed."
}
Expand-Archive -LiteralPath $archive -DestinationPath $staging
$bundle = Join-Path $staging $archiveName
foreach ($file in @('scrcpy.exe', 'scrcpy-server', 'adb.exe', 'SDL3.dll', 'AdbWinApi.dll', 'AdbWinUsbApi.dll')) {
    if (-not (Test-Path -LiteralPath (Join-Path $bundle $file) -PathType Leaf)) {
        throw "Official bundle is missing $file. Bundle not installed."
    }
}
$parent = Split-Path -Parent $destinationPath
New-Item -ItemType Directory -Force -Path $parent | Out-Null
Copy-Item -LiteralPath $bundle -Destination $destinationPath -Recurse
Write-Host "Bundled official scrcpy $version at $destinationPath (SHA-256 verified)."
