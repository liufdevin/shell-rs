$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { exit 1 }
$plan = @([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__SELECTION__')) | ConvertFrom-Json)
if ($plan.Count -eq 0 -or $plan.Count -gt 32) { exit 1 }
$seen = @{}
foreach ($item in $plan) {
    $id = [string]$item.instance_id
    if ($id -notmatch '^USB\\[A-Za-z0-9\\&_.#{}+\-]+$' -or $id.Length -gt 512 -or $seen.ContainsKey($id) -or $item.present -ne $false -or $item.port -cnotmatch '^COM[1-9][0-9]{0,4}$') { exit 1 }
    $seen[$id] = $true
}
$system = [Environment]::GetFolderPath([Environment+SpecialFolder]::System)
$pnputil = Join-Path $system 'pnputil.exe'
if (-not (Test-Path -LiteralPath $pnputil -PathType Leaf)) { exit 1 }
$client = New-Object Net.Sockets.TcpClient
try {
    $connect = $client.ConnectAsync('127.0.0.1', __PORT__)
    if (-not $connect.Wait(5000)) { exit 1 }
    $connect.GetAwaiter().GetResult()
    $stream = $client.GetStream()
    $stream.WriteTimeout = 5000
    $writer = New-Object IO.StreamWriter($stream, (New-Object Text.UTF8Encoding($false)))
    $writer.AutoFlush = $true
    $writer.WriteLine('__TOKEN__')
    $deadline = [DateTime]::UtcNow.AddSeconds(90)
    foreach ($item in $plan) {
        $state = 'failed'
        $detail = ''
        try {
            if ([DateTime]::UtcNow -gt $deadline) { throw 'Operation time limit reached; device was not processed.' }
            $all = @(Get-PnpDevice -ErrorAction Stop)
            $match = @($all | Where-Object { $_.InstanceId -eq $item.instance_id })
            if ($match.Count -eq 0) {
                $state = 'gone'
            } elseif ($match.Count -ne 1) {
                $state = 'changed'
            } else {
                $current = Read-SerialDevice $match[0]
                if ($null -eq $current -or $current.port -cne $item.port) {
                    $state = 'changed'
                } elseif ($current.present -eq $true) {
                    $state = 'present'
                } elseif ($current.present -ne $false) {
                    throw 'Device presence could not be verified; nothing was removed.'
                } else {
                    $live = @(Get-PnpDevice -PresentOnly -ErrorAction Stop | Where-Object { $_.InstanceId -eq $item.instance_id })
                    if ($live.Count -ne 0) {
                        $state = 'present'
                    } else {
                        if ([DateTime]::UtcNow -gt $deadline) { throw 'Operation time limit reached; device was not processed.' }
                        $start = New-Object Diagnostics.ProcessStartInfo
                        $start.FileName = $pnputil
                        $start.Arguments = '/remove-device "' + $item.instance_id + '"'
                        $start.UseShellExecute = $false
                        $start.CreateNoWindow = $true
                        $start.RedirectStandardOutput = $true
                        $start.RedirectStandardError = $true
                        $process = New-Object Diagnostics.Process
                        $process.StartInfo = $start
                        try {
                            [void]$process.Start()
                            $stdout = $process.StandardOutput.ReadToEndAsync()
                            $stderr = $process.StandardError.ReadToEndAsync()
                            if (-not $process.WaitForExit(10000)) {
                                $process.Kill()
                                throw 'Removal timed out; rescan to verify the device state.'
                            }
                            $code = $process.ExitCode
                            if ($code -eq 3010) {
                                $state = 'reboot_required'
                            } elseif ($code -ne 0) {
                                throw ('pnputil exit code ' + $code + ': ' + $stdout.Result + ' ' + $stderr.Result)
                            } else {
                                $remaining = @(Get-PnpDevice -ErrorAction Stop | Where-Object { $_.InstanceId -eq $item.instance_id })
                                if ($remaining.Count -eq 0) { $state = 'removed' }
                                else { throw 'The device is still registered; rescan before retrying.' }
                            }
                        } finally { $process.Dispose() }
                    }
                }
            }
        } catch {
            $detail = [regex]::Replace([string]$_.Exception.Message, '[\x00-\x1f\x7f]', ' ')
            if ($detail.Length -gt 512) { $detail = $detail.Substring(0, 512) }
        }
        $row = [pscustomobject]@{ instance_id = [string]$item.instance_id; state = $state; detail = $detail }
        $writer.WriteLine(($row | ConvertTo-Json -Depth 3 -Compress))
    }
    $writer.Dispose()
} finally { $client.Dispose() }
