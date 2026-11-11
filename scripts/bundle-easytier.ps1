param([string]$Destination = './dist')
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$cache = Join-Path $root 'target/remote-backend'
$null = New-Item -ItemType Directory -Path $cache -Force
$archive = Join-Path $cache 'easytier-windows-x86_64-v2.6.4.zip'
$archiveHash = '27af91e270e554709b048bd32327fefd2dfce5062ae1e8701af7550c6f525f84'
if (-not (Test-Path -LiteralPath $archive) -or (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $archiveHash) {
    Invoke-WebRequest -Uri 'https://github.com/EasyTier/EasyTier/releases/download/v2.6.4/easytier-windows-x86_64-v2.6.4.zip' -OutFile $archive
}
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant() -ne $archiveHash) { throw 'EasyTier archive checksum mismatch' }
$null = New-Item -ItemType Directory -Path $Destination -Force
$destinationPath = (Resolve-Path -LiteralPath $Destination).Path
$hashes = @{
    'easytier-core.exe'='da7eb2d24b5416f3d3407636949e964a0750e3f9dc53a828cb6799a57ead445d'
    'easytier-cli.exe'='d8783e851e944b44a9b71b39fd02f227ec0a2a82b3165c55ead5dd32dcde53a1'
    'wintun.dll'='e5da8447dc2c320edc0fc52fa01885c103de8c118481f683643cacc3220dafce'
}
Add-Type -AssemblyName System.IO.Compression.FileSystem
$package = [IO.Compression.ZipFile]::OpenRead($archive)
try {
    foreach ($name in $hashes.Keys) {
        $entries = @($package.Entries | Where-Object { $_.Name -eq $name })
        if ($entries.Count -ne 1 -or $entries[0].Length -gt 100MB) { throw 'Invalid EasyTier archive' }
        $target = Join-Path $destinationPath $name
        [IO.Compression.ZipFileExtensions]::ExtractToFile($entries[0], $target, $true)
        if ((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hashes[$name]) { throw "Checksum mismatch: $name" }
        Write-Output "Verified $name"
    }
} finally { $package.Dispose() }
$packetHash = 'ab3cce674f5216895fd26a073771f82b05d4c8b214a89f0f288a59774a06b14b'
$packet = Join-Path $cache 'Packet-WinPcap.dll'
if (-not (Test-Path -LiteralPath $packet) -or (Get-FileHash -LiteralPath $packet -Algorithm SHA256).Hash.ToLowerInvariant() -ne $packetHash) {
    $winpcap = Join-Path $cache 'WinPcap_4_1_3.exe'
    $installerHash = 'fc4623b113a1f603c0d9ad5f83130bd6de1c62b973be9892305132389c8588de'
    if (-not (Test-Path -LiteralPath $winpcap)) {
        Invoke-WebRequest -Uri 'https://www.winpcap.org/install/bin/WinPcap_4_1_3.exe' -OutFile $winpcap
    }
    if ((Get-FileHash -LiteralPath $winpcap -Algorithm SHA256).Hash.ToLowerInvariant() -ne $installerHash) { throw 'WinPcap archive checksum mismatch' }
    $extractor = Get-Command 7z.exe -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Source
    if (-not $extractor) { $extractor = Join-Path $cache '7zip/7z.exe' }
    if (-not (Test-Path -LiteralPath $extractor)) { throw '7-Zip is needed to extract the compatible WinPcap Packet.dll. The installer is never executed.' }
    $output = Join-Path $cache ('packet-extract-' + [Guid]::NewGuid().ToString('N'))
    & $extractor e $winpcap ('-o' + $output) -aou -y | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Cannot extract WinPcap archive' }
    $found = Get-ChildItem -LiteralPath $output -Filter 'Packet*.dll' | Where-Object { (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() -eq $packetHash } | Select-Object -First 1
    if (-not $found) { throw 'WinPcap x64 Packet.dll checksum mismatch' }
    Copy-Item -LiteralPath $found.FullName -Destination $packet -Force
}
Copy-Item -LiteralPath $packet -Destination (Join-Path $destinationPath 'Packet.dll') -Force
$actualPacket = (Get-FileHash -LiteralPath (Join-Path $destinationPath 'Packet.dll') -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualPacket -ne $packetHash) { throw 'Packet.dll checksum mismatch' }
Write-Output 'Verified Packet.dll (WinPcap compatibility library, no driver installed)'
$licenses = Join-Path $destinationPath 'licenses'
$null = New-Item -ItemType Directory -Path $licenses -Force
Copy-Item -LiteralPath (Join-Path $root 'packaging/windows/EasyTier-LICENSE'), (Join-Path $root 'packaging/windows/Wintun-LICENSE.txt') -Destination $licenses -Force
Copy-Item -LiteralPath (Join-Path $root 'packaging/windows/WinPcap-LICENSE.html'), (Join-Path $root 'LICENSE') -Destination $licenses -Force
Copy-Item -LiteralPath (Join-Path $root 'packaging/windows/REMOTE-THIRD-PARTY.md') -Destination $licenses -Force
