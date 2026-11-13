$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-StrictMode -Version Latest
$env:PSModulePath = Join-Path $PSHOME 'Modules'
$request = [Console]::In.ReadToEnd() | ConvertFrom-Json
$pathsOnly = $request.PSObject.Properties.Name -contains 'pathsOnly' -and $request.pathsOnly
Add-Type -TypeDefinition @'
using System;
using System.Diagnostics;
using System.IO;
using System.Net;
using System.Net.NetworkInformation;
using System.Net.Sockets;
using System.Text;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
namespace ShellLfRemoteDiagnostics {
    public sealed class Measurement {
        public string Status { get; set; }
        public double? LatencyMs { get; set; }
    }
    public static class Probe {
        [StructLayout(LayoutKind.Sequential)] public struct FileTime { public uint Low, High; }
        [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(uint access, bool inherit, int id);
        [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder path, ref int length);
        [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetProcessTimes(IntPtr process, out FileTime created, out FileTime exited, out FileTime kernel, out FileTime user);
        [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetExitCodeProcess(IntPtr process, out uint code);
        [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
        public static bool OwnedCore(int id, string started, string expected) {
            var process = OpenProcess(0x1000, false, id);
            if (process == IntPtr.Zero) return false;
            try {
                uint code;
                FileTime created, exited, kernel, user;
                var path = new StringBuilder(32768);
                int length = path.Capacity;
                if (!GetExitCodeProcess(process, out code) || code != 259 || !QueryFullProcessImageName(process, 0, path, ref length) ||
                    !GetProcessTimes(process, out created, out exited, out kernel, out user)) return false;
                var ticks = DateTime.FromFileTimeUtc(((long)created.High << 32) | created.Low).Ticks.ToString(System.Globalization.CultureInfo.InvariantCulture);
                return ticks == started && String.Equals(Path.GetFullPath(path.ToString()), Path.GetFullPath(expected), StringComparison.OrdinalIgnoreCase);
            } catch (Exception) { return false; }
            finally { CloseHandle(process); }
        }
        public static async Task<Measurement> Tcp(string source, string target, int port, int timeout) {
            using (var client = new TcpClient(AddressFamily.InterNetwork)) {
                var timer = Stopwatch.StartNew();
                try {
                    client.Client.Bind(new IPEndPoint(IPAddress.Parse(source), 0));
                    var connection = client.ConnectAsync(IPAddress.Parse(target), port);
                    if (await Task.WhenAny(connection, Task.Delay(timeout)).ConfigureAwait(false) != connection) {
                        client.Close();
                        var observation = connection.ContinueWith(failed => failed.Exception.Handle(ignored => true), TaskContinuationOptions.OnlyOnFaulted);
                        GC.KeepAlive(observation);
                        return new Measurement { Status = "timeout" };
                    }
                    await connection.ConfigureAwait(false);
                    return new Measurement { Status = "open", LatencyMs = Math.Round(timer.Elapsed.TotalMilliseconds, 1) };
                } catch (SocketException error) {
                    return new Measurement { Status = error.SocketErrorCode == SocketError.ConnectionRefused ? "refused" : error.SocketErrorCode == SocketError.TimedOut ? "timeout" : "error" };
                } catch (Exception) { return new Measurement { Status = "error" }; }
            }
        }
        public static async Task<Measurement> Ping(string target) {
            using (var ping = new Ping()) {
                try {
                    var response = await ping.SendPingAsync(IPAddress.Parse(target), 800).ConfigureAwait(false);
                    return new Measurement { Status = response.Status == IPStatus.Success ? "ok" : response.Status == IPStatus.TimedOut ? "timeout" : "error",
                        LatencyMs = response.Status == IPStatus.Success ? (double?)response.RoundtripTime : null };
                } catch (Exception) { return new Measurement { Status = "error" }; }
            }
        }
        public static async Task<string> ReadBounded(StreamReader reader) {
            var text = new StringBuilder();
            var buffer = new char[4096];
            int count;
            while ((count = await reader.ReadAsync(buffer, 0, buffer.Length).ConfigureAwait(false)) != 0) {
                if (text.Length + count > 2097152) throw new IOException("diagnostic_limit");
                text.Append(buffer, 0, count);
            }
            return text.ToString();
        }
    }
}
'@

function Core-Json([ValidateSet('peer','route')][string]$Kind) {
    $executable = Join-Path $request.packageRoot 'easytier-cli.exe'
    if ((Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToLowerInvariant() -ne 'd8783e851e944b44a9b71b39fd02f227ec0a2a82b3165c55ead5dd32dcde53a1') { throw 'backend_hash_mismatch' }
    $process = New-Object Diagnostics.Process
    $process.StartInfo.FileName = $executable
    $process.StartInfo.WorkingDirectory = $request.packageRoot
    $process.StartInfo.Arguments = '-p 127.0.0.1:15889 -o json ' + $Kind
    $process.StartInfo.UseShellExecute = $false
    $process.StartInfo.CreateNoWindow = $true
    $process.StartInfo.RedirectStandardOutput = $true
    $process.StartInfo.RedirectStandardError = $true
    foreach ($key in @($process.StartInfo.EnvironmentVariables.Keys)) {
        if ($key.StartsWith('ET_') -or $key -in @('HTTP_PROXY','HTTPS_PROXY','ALL_PROXY')) { $process.StartInfo.EnvironmentVariables.Remove($key) }
    }
    $started = $false
    try {
        $started = $process.Start()
        if (-not $started) { throw 'diagnostic_failed' }
        $output = [ShellLfRemoteDiagnostics.Probe]::ReadBounded($process.StandardOutput)
        $errors = [ShellLfRemoteDiagnostics.Probe]::ReadBounded($process.StandardError)
        if (-not $process.WaitForExit(3000) -or $process.ExitCode -ne 0 -or -not $output.Wait(500) -or -not $errors.Wait(500)) { throw 'diagnostic_failed' }
        $value = $output.GetAwaiter().GetResult() | ConvertFrom-Json
        if (@($value).Count -gt 10000) { throw 'diagnostic_limit' }
        return $value
    } finally {
        if ($started -and -not $process.HasExited) { $process.Kill(); [void]$process.WaitForExit(1000) }
        $process.Dispose()
    }
}

$local = @{adapterReady=$false; coreRunning=$false; rpcReady=$false}
foreach ($adapter in [Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces()) {
    if ($adapter.Name -eq 'bjssh0' -and $adapter.OperationalStatus.ToString() -eq 'Up') {
        $addresses = @($adapter.GetIPProperties().UnicastAddresses | ForEach-Object { $_.Address.ToString() })
        if ($addresses -contains $request.virtualIp) { $local.adapterReady = $true }
    }
}
try {
    $status = Get-Content -LiteralPath (Join-Path $request.directory 'status.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    $expected = [IO.Path]::GetFullPath((Join-Path $request.packageRoot 'easytier-core.exe'))
    $local.coreRunning = [ShellLfRemoteDiagnostics.Probe]::OwnedCore([int]$status.corePid, [string]$status.coreStartedTicks, $expected)
} catch { $local.coreRunning = $false }
$routes = @(); $peers = @()
if ($local.coreRunning) {
    try {
        $rpc = [ShellLfRemoteDiagnostics.Probe]::Tcp('127.0.0.1', '127.0.0.1', 15889, 500).GetAwaiter().GetResult()
        $local.rpcReady = $rpc.Status -eq 'open'
    } catch { $local.rpcReady = $false }
}
$results = New-Object 'System.Collections.Generic.List[object]'
$devices = @(if (-not $pathsOnly) { $request.devices })
if ($devices.Count -gt 100) { throw 'diagnostic_limit' }
for ($offset = 0; $offset -lt $devices.Count; $offset += 16) {
    $batch = @($devices | Select-Object -Skip $offset -First 16)
    $pending = @()
    foreach ($device in $batch) {
        $tcp = $null; $icmp = $null
        if ($local.adapterReady -and $local.coreRunning -and $local.rpcReady) {
            $tcp = [ShellLfRemoteDiagnostics.Probe]::Tcp($request.virtualIp, $device.virtualIp, 22, 3000)
            $icmp = [ShellLfRemoteDiagnostics.Probe]::Ping($device.virtualIp)
        }
        $pending += @{device=$device; tcp=$tcp; icmp=$icmp}
    }
    $tasks = [Threading.Tasks.Task[]]@($pending | ForEach-Object {
        if ($null -ne $_.tcp) { $_.tcp }
        if ($null -ne $_.icmp) { $_.icmp }
    })
    if ($tasks.Count) { try { [void][Threading.Tasks.Task]::WaitAll($tasks, 3500) } catch {} }
    foreach ($probe in $pending) {
        $tcp = @{status='not_tested'; latencyMs=$null}
        $icmp = @{status='not_tested'; latencyMs=$null}
        if ($null -ne $probe.tcp) {
            try {
                if ($probe.tcp.IsCompleted) {
                    $measurement = $probe.tcp.GetAwaiter().GetResult()
                    $tcp.status = $measurement.Status; $tcp.latencyMs = $measurement.LatencyMs
                } else { $tcp.status = 'timeout' }
            } catch { $tcp.status = 'error' }
        }
        if ($null -ne $probe.icmp) {
            try {
                if ($probe.icmp.IsCompleted) {
                    $measurement = $probe.icmp.GetAwaiter().GetResult()
                    $icmp.status = $measurement.Status; $icmp.latencyMs = $measurement.LatencyMs
                } else { $icmp.status = 'timeout' }
            } catch { $icmp.status = 'error' }
        }
        $results.Add(@{deviceId=$probe.device.deviceId; virtualIp=$probe.device.virtualIp; tcp22=$tcp; icmp=$icmp})
    }
}
if ($local.coreRunning -and $local.rpcReady -and (-not $pathsOnly -or $local.adapterReady)) {
    try {
        $routes = @(Core-Json 'route')
        $peers = @(Core-Json 'peer')
    } catch { $routes = @(); $peers = @(); $local.rpcReady = $false }
}
$report = @{local=$local; devices=@($results.ToArray()); routes=@($routes); peers=@($peers)}
[IO.File]::WriteAllText($request.output, ($report | ConvertTo-Json -Depth 12 -Compress), (New-Object Text.UTF8Encoding($false)))
