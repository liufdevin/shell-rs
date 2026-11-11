param([Parameter(Mandatory = $true)][string]$StateDirectory)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
$env:PSModulePath = Join-Path $PSHOME 'Modules'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$core = $null
$context = $null
$token = ''
$failure = ''
$mutex = $null
$locked = $false
$job = [IntPtr]::Zero
$firewall = 'ShellLF-Remote-' + [IO.Path]::GetFileName($StateDirectory)

function Status([string]$State) {
    @{state=$State; error=$failure} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $StateDirectory 'status.tmp') -Encoding UTF8
    Move-Item -LiteralPath (Join-Path $StateDirectory 'status.tmp') -Destination (Join-Path $StateDirectory 'status.json') -Force
}

function Api([string]$Path, [int]$Timeout) {
    $request = @{
        Uri = $context.serverUrl + $Path; Method = 'POST'; ContentType = 'application/json; charset=utf-8'
        Headers = @{'X-Baijie-Request'='1'; 'X-Baijie-Client'='native'; Authorization=('Bearer ' + $token)}
        Body = (@{clientId=$context.clientId; networkId=$context.networkId} | ConvertTo-Json -Compress)
        MaximumRedirection = 0; TimeoutSec = $Timeout; ErrorAction = 'Stop'
    }
    Invoke-RestMethod @request
}

function Range([string]$Cidr) {
    $parts = $Cidr.Split('/')
    $prefix = [int]$parts[1]
    if ($prefix -lt 0 -or $prefix -gt 32) { throw 'invalid_profile' }
    $octets = [Net.IPAddress]::Parse($parts[0]).GetAddressBytes()
    if ($octets.Length -ne 4) { throw 'invalid_profile' }
    $number = [uint64]$octets[0]*16777216 + [uint64]$octets[1]*65536 + [uint64]$octets[2]*256 + [uint64]$octets[3]
    $size = [uint64][Math]::Pow(2, 32-$prefix)
    $first = [uint64]([Math]::Floor($number/$size)*$size)
    @($first, ($first+$size-1))
}

function Confirm-Renewal([object]$Renewed, [object]$Original, [string]$ClientId, [string]$NetworkId) {
    if ($Renewed.clientId -ne $ClientId -or $Renewed.networkId -ne $NetworkId) { throw 'invalid_profile' }
    if ($Renewed.profileHash -ne $Original.profileHash -or $Renewed.virtualIp -ne $Original.virtualIp) { throw 'profile_changed_reconnect' }
    if ([long]$Renewed.expiresIn -lt 1 -or [long]$Renewed.expiresIn -gt 3600) { throw 'invalid_lease' }
}

try {
    $context = Get-Content -LiteralPath (Join-Path $StateDirectory 'context.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $mutex = New-Object Threading.Mutex($false, ('Local\BaijieRemoteSsh-' + $identity.User.Value))
    try { $locked = $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $locked = $true }
    if (-not $locked) { throw 'network_in_use' }
    $secure = ConvertTo-SecureString $context.protectedToken
    try { $token = (New-Object PSCredential('token', $secure)).GetNetworkCredential().Password } finally { $secure.Dispose() }
    $hashes = @{
        'Packet.dll'='ab3cce674f5216895fd26a073771f82b05d4c8b214a89f0f288a59774a06b14b'
        'easytier-core.exe'='da7eb2d24b5416f3d3407636949e964a0750e3f9dc53a828cb6799a57ead445d'
        'easytier-cli.exe'='d8783e851e944b44a9b71b39fd02f227ec0a2a82b3165c55ead5dd32dcde53a1'
        'wintun.dll'='e5da8447dc2c320edc0fc52fa01885c103de8c118481f683643cacc3220dafce'
    }
    foreach ($name in $hashes.Keys) {
        if ((Get-FileHash -LiteralPath (Join-Path $context.packageRoot $name) -Algorithm SHA256).Hash.ToLowerInvariant() -ne $hashes[$name]) { throw 'backend_hash_mismatch' }
    }
    $authorization = $context.authorization
    $config = $authorization.files.'network.json' | ConvertFrom-Json
    $text = [string]$authorization.files.'client.toml'
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $actual = ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($text)))).Replace('-','').ToLowerInvariant() } finally { $sha.Dispose() }
    if ($actual -ne $authorization.profileHash) { throw 'profile_hash_mismatch' }
    if ($config.role -ne 'client' -or $config.rpc_portal -ne '127.0.0.1:15889' -or $config.virtual_ip.Split('/')[0] -ne $authorization.virtualIp) { throw 'invalid_profile' }
    $wanted = Range $config.virtual_ip
    foreach ($route in Get-NetRoute -AddressFamily IPv4 -ErrorAction Stop) {
        if ($route.DestinationPrefix -eq '0.0.0.0/0') { continue }
        $range = Range $route.DestinationPrefix
        if ($range[0] -le $wanted[1] -and $wanted[0] -le $range[1]) { throw 'route_conflict' }
    }
    if (Test-Path -LiteralPath (Join-Path $StateDirectory 'stop')) { throw 'network_cancelled' }
    [IO.File]::WriteAllText((Join-Path $StateDirectory 'client.toml'), $text, (New-Object Text.UTF8Encoding($false)))
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $elapsed = (([long][Environment]::TickCount - [long]$context.issuedAtTick + 4294967296) % 4294967296)/1000
    $lease = [long]$authorization.expiresIn - $elapsed
    if ($lease -le 0 -or $lease -gt 3600) { throw 'lease_expired' }
    $renewAt = [Math]::Min(60, $lease/3)
    $executable = Join-Path $context.packageRoot 'easytier-core.exe'
    New-NetFirewallRule -Name $firewall -DisplayName 'ShellLF remote network' -Program $executable -Direction Inbound -Action Allow -Profile Any | Out-Null
    $core = New-Object Diagnostics.Process
    $core.StartInfo.FileName = $executable
    $core.StartInfo.WorkingDirectory = $context.packageRoot
    $core.StartInfo.Arguments = '-c "' + (Join-Path $StateDirectory 'client.toml') + '" --rpc-portal 127.0.0.1:15889 --disable-env-parsing'
    $core.StartInfo.UseShellExecute = $false
    $core.StartInfo.CreateNoWindow = $true
    $core.StartInfo.RedirectStandardOutput = $true
    $core.StartInfo.RedirectStandardError = $true
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ShellLfNetworkJob {
    [StructLayout(LayoutKind.Sequential)] public struct Basic {
        public long ProcessTime, JobTime; public uint Flags;
        public UIntPtr Min, Max; public uint Count; public UIntPtr Affinity; public uint Priority, Scheduling;
    }
    [StructLayout(LayoutKind.Sequential)] public struct Io {
        public ulong ReadOps, WriteOps, OtherOps, ReadBytes, WriteBytes, OtherBytes;
    }
    [StructLayout(LayoutKind.Sequential)] public struct Extended {
        public Basic Limits; public Io Counters; public UIntPtr ProcessMemory, JobMemory, PeakProcess, PeakJob;
    }
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] public static extern IntPtr CreateJobObject(IntPtr attributes, string name);
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool SetInformationJobObject(IntPtr job, int kind, ref Extended info, uint length);
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
    [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr handle);
}
'@
    $job = [ShellLfNetworkJob]::CreateJobObject([IntPtr]::Zero, $null)
    $limit = New-Object ShellLfNetworkJob+Extended
    $basic = New-Object ShellLfNetworkJob+Basic
    $basic.Flags = 0x2000
    $limit.Limits = $basic
    if ($job -eq [IntPtr]::Zero -or -not [ShellLfNetworkJob]::SetInformationJobObject($job, 9, [ref]$limit, [uint32][Runtime.InteropServices.Marshal]::SizeOf($limit))) { throw 'network_stopped' }
    foreach ($key in @($core.StartInfo.EnvironmentVariables.Keys)) { if ($key.StartsWith('ET_')) { $core.StartInfo.EnvironmentVariables.Remove($key) } }
    if (-not $core.Start()) { throw 'core_exited' }
    if (-not [ShellLfNetworkJob]::AssignProcessToJobObject($job, $core.Handle)) { throw 'network_stopped' }
    $output = $core.StandardOutput.ReadLineAsync()
    $errors = $core.StandardError.ReadLineAsync()
    $running = $false
    $statusAt = 0
    while (-not (Test-Path -LiteralPath (Join-Path $StateDirectory 'stop'))) {
        $parent = Get-Process -Id $context.parentPid -ErrorAction SilentlyContinue
        if ($null -eq $parent -or $parent.StartTime.ToUniversalTime().Ticks -ne [long]$context.parentStarted) { break }
        if ($core.HasExited) { throw 'core_exited' }
        if ($clock.Elapsed.TotalSeconds -ge $lease) { throw 'lease_expired' }
        if (-not $running) {
            $adapter = Get-NetIPAddress -AddressFamily IPv4 -IPAddress $authorization.virtualIp -ErrorAction SilentlyContinue
            if ($adapter) { Status 'running'; $running = $true }
        }
        if ($running -and $clock.Elapsed.TotalSeconds -ge $statusAt) { Status 'running'; $statusAt = $clock.Elapsed.TotalSeconds + 1 }
        if ($clock.Elapsed.TotalSeconds -ge $renewAt) {
            $renewStarted = $clock.Elapsed.TotalSeconds
            try {
                $remaining = [int][Math]::Floor($lease - $renewStarted)
                if ($remaining -lt 1) { throw 'lease_expired' }
                $renewed = Api '/api/client/ssh/session' ([Math]::Min(20, $remaining))
                if ($clock.Elapsed.TotalSeconds -ge $lease) { throw 'lease_expired' }
                Confirm-Renewal $renewed $authorization $context.clientId $context.networkId
                $lease = $renewStarted + [long]$renewed.expiresIn
                $renewAt = $clock.Elapsed.TotalSeconds + [Math]::Max(1, [Math]::Min(60, [long]$renewed.expiresIn/3))
            } catch {
                $response = $_.Exception.PSObject.Properties['Response']
                if ($null -ne $response -and $null -ne $response.Value -and [int]$response.Value.StatusCode -in @(400,401,403,404,409)) { throw 'authorization_revoked' }
                if ($_.Exception.Message -in @('lease_expired','invalid_lease','invalid_profile','profile_changed_reconnect')) { throw }
                $renewAt = $clock.Elapsed.TotalSeconds + 5
            }
        }
        if ($null -ne $output -and $output.IsCompleted) {
            if ($null -eq $output.GetAwaiter().GetResult()) { $output = $null } else { $output = $core.StandardOutput.ReadLineAsync() }
        }
        if ($null -ne $errors -and $errors.IsCompleted) {
            if ($null -eq $errors.GetAwaiter().GetResult()) { $errors = $null } else { $errors = $core.StandardError.ReadLineAsync() }
        }
        Start-Sleep -Milliseconds 100
    }
} catch {
    $failure = if ($_.Exception.Message -in @('network_in_use','backend_hash_mismatch','profile_hash_mismatch','invalid_profile','route_conflict','lease_expired','invalid_lease','profile_changed_reconnect','core_exited','authorization_revoked','network_cancelled')) { $_.Exception.Message } else { 'network_stopped' }
} finally {
    if ($null -ne $core) {
        try { if (-not $core.HasExited) { $core.Kill(); $core.WaitForExit() } } catch {}
        $core.Dispose()
    }
    if ($job -ne [IntPtr]::Zero) { [ShellLfNetworkJob]::CloseHandle($job) | Out-Null }
    Remove-NetFirewallRule -Name $firewall -ErrorAction SilentlyContinue
    if ($token -and $null -ne $context) { try { $null = Api '/api/client/ssh/disconnect' 5 } catch {} }
    $token = ''
    foreach ($name in @('client.toml','context.json')) { Remove-Item -LiteralPath (Join-Path $StateDirectory $name) -Force -ErrorAction SilentlyContinue }
    if ($locked) { $mutex.ReleaseMutex() }
    if ($null -ne $mutex) { $mutex.Dispose() }
    Status 'stopped'
}
