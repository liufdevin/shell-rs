$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$env:PSModulePath = Join-Path $PSHOME 'Modules'
[Console]::InputEncoding = New-Object Text.UTF8Encoding($false)
$inputData = [Console]::In.ReadToEnd() | ConvertFrom-Json
$directory = [IO.Path]::GetFullPath($inputData.directory)
if ($directory.Contains('"')) { throw 'invalid_directory' }
$worker = Join-Path $directory 'worker.ps1'
$arguments = '-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' + $worker + '" -StateDirectory "' + $directory + '"'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $process = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList $arguments -WindowStyle Hidden -PassThru
} else {
    $process = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList $arguments -Verb RunAs -WindowStyle Hidden -PassThru
}
if ($null -eq $process) { throw 'network_launch_failed' }
