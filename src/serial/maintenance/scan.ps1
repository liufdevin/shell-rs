[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$devices = @(Get-PnpDevice -ErrorAction Stop | Where-Object { $_.Class -eq 'Ports' -and $_.InstanceId -match '^USB\\' })
if ($devices.Count -gt 256) { throw 'Too many USB serial devices; scan aborted.' }
$rows = @()
foreach ($device in $devices) {
    $row = Read-SerialDevice $device
    if ($null -ne $row) { $rows += $row }
}
ConvertTo-Json -InputObject @($rows) -Depth 4 -Compress
