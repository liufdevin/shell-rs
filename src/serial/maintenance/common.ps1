$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$env:PSModulePath = Join-Path $PSHOME 'Modules'
Import-Module (Join-Path $PSHOME 'Modules\PnpDevice\PnpDevice.psd1') -ErrorAction Stop

function Read-SerialDevice($Device) {
    $id = [string]$Device.InstanceId
    if ($Device.Class -ne 'Ports' -or $id -notmatch '^USB\\[A-Za-z0-9\\&_.#{}+\-]+$' -or $id.Length -gt 512) { return $null }
    $class = Get-PnpDeviceProperty -InstanceId $id -KeyName 'DEVPKEY_Device_ClassGuid' -ErrorAction Stop
    if (([string]$class.Data).Trim('{}') -ne '4d36e978-e325-11ce-bfc1-08002be10318') { return $null }
    $key = 'Registry::HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Enum\' + $id + '\Device Parameters'
    $port = [string](Get-ItemProperty -LiteralPath $key -Name PortName -ErrorAction Stop).PortName
    if ($port -cnotmatch '^COM[1-9][0-9]{0,4}$' -or [int]$port.Substring(3) -gt 65535) { return $null }
    $presence = Get-PnpDeviceProperty -InstanceId $id -KeyName 'DEVPKEY_Device_IsPresent' -ErrorAction Stop
    $present = $null
    if ($presence.Data -is [bool]) { $present = $presence.Data }
    $name = [regex]::Replace([string]$Device.FriendlyName, '[\x00-\x1f\x7f]', ' ')
    if ($name.Length -gt 512) { $name = $name.Substring(0, 512) }
    return [pscustomobject]@{ instance_id = $id; port = $port; name = $name; present = $present }
}
