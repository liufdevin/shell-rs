$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$env:PSModulePath = Join-Path $PSHOME 'Modules'
[Console]::InputEncoding = New-Object Text.UTF8Encoding($false)
$context = [Console]::In.ReadToEnd() | ConvertFrom-Json
$directory = [IO.Path]::GetFullPath($context.directory)
$item = Get-Item -LiteralPath $directory -Force
if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'invalid_directory' }
$acl = New-Object Security.AccessControl.DirectorySecurity
$acl.SetAccessRuleProtection($true, $false)
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
foreach ($sid in @($identity.User, (New-Object Security.Principal.SecurityIdentifier('S-1-5-18')), (New-Object Security.Principal.SecurityIdentifier('S-1-5-32-544')))) {
    $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')))
}
Set-Acl -LiteralPath $directory -AclObject $acl
$secret = ConvertTo-SecureString $context.token -AsPlainText -Force
try { $protected = ConvertFrom-SecureString $secret } finally { $secret.Dispose() }
$context.PSObject.Properties.Remove('token')
$context | Add-Member -NotePropertyName protectedToken -NotePropertyValue $protected
$context | Add-Member -NotePropertyName parentStarted -NotePropertyValue (Get-Process -Id $context.parentPid).StartTime.ToUniversalTime().Ticks
$context | Add-Member -NotePropertyName issuedAtTick -NotePropertyValue ([Environment]::TickCount)
$context | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $directory 'context.json') -Encoding UTF8
