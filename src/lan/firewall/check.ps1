function Test-Any($value) {
    $values = @($value)
    return $values.Count -eq 0 -or ($values.Count -eq 1 -and ([string]$values[0] -in @('', '*', 'Any')))
}
function Test-Rule($suffix, $protocol, $port) {
    $found = @($rules | Where-Object { $_.Name -eq "$rulePrefix-$suffix" })
    if ($found.Count -ne 1) { return $false }
    $rule = $found[0]
    return $rule.Enabled -and [int]$rule.Direction -eq 1 -and [int]$rule.Action -eq 1 -and [int]$rule.Profiles -eq 2 -and
        -not $rule.EdgeTraversal -and [int]$rule.EdgeTraversalOptions -eq 0 -and $rule.Grouping -eq $group -and $rule.ApplicationName -ieq $program -and
        [int]$rule.Protocol -eq $protocol -and [string]$rule.LocalPorts -eq [string]$port -and (Test-Any $rule.RemotePorts) -and
        (Test-Any $rule.LocalAddresses) -and [string]$rule.RemoteAddresses -ieq 'LocalSubnet' -and
        (Test-Any $rule.ServiceName) -and (Test-Any $rule.Interfaces) -and [string]$rule.InterfaceTypes -eq 'All' -and
        [int]$rule.SecureFlags -eq 0 -and (Test-Any $rule.LocalAppPackageId) -and (Test-Any $rule.LocalUserAuthorizedList) -and
        (Test-Any $rule.RemoteUserAuthorizedList) -and (Test-Any $rule.RemoteMachineAuthorizedList)
}
function Test-PortOverlap($value, $port) {
    if (Test-Any $value) { return $true }
    foreach ($part in ([string]$value -split ',')) {
        if ($part -match '^\d+$') { if ([int]$part -eq $port) { return $true } }
        elseif ($part -match '^(\d+)-(\d+)$') { if ($port -ge [int]$Matches[1] -and $port -le [int]$Matches[2]) { return $true } }
        else { return $true }
    }
    return $false
}
try {
    $policy = New-Object -ComObject HNetCfg.FwPolicy2
    $rules = @($policy.Rules)
    $blocks = 0
    foreach ($rule in $rules) {
        if (-not $rule.Enabled -or [int]$rule.Direction -ne 1 -or [int]$rule.Action -ne 0 -or ([int]$rule.Profiles -band 2) -eq 0) { continue }
        if (-not (Test-Any $rule.ApplicationName) -and [Environment]::ExpandEnvironmentVariables($rule.ApplicationName) -ine $program) { continue }
        if ([int]$rule.Protocol -eq 256 -or ([int]$rule.Protocol -eq 6 -and (Test-PortOverlap $rule.LocalPorts $tcpPort)) -or
            ([int]$rule.Protocol -eq 17 -and (Test-PortOverlap $rule.LocalPorts $udpPort))) { $blocks++ }
    }
    $report = @{
        private_network = [bool](([int]$policy.CurrentProfileTypes -band 2) -ne 0)
        firewall_enabled = [bool]$policy.FirewallEnabled(2)
        inbound_allowed = -not [bool]$policy.BlockAllInboundTraffic(2) -and [int]$policy.LocalPolicyModifyState -ne 2
        local_rules_allowed = [int]$policy.LocalPolicyModifyState -eq 0
        tcp_rule = [bool](Test-Rule 'TCP' 6 $tcpPort)
        udp_rule = [bool](Test-Rule 'UDP' 17 $udpPort)
        possible_blocks = [int]$blocks
    }
    [Console]::Out.Write(($report | ConvertTo-Json -Compress))
    exit 0
} catch {
    [Console]::Error.Write(($_.Exception.Message + ' ' + $_.InvocationInfo.PositionMessage))
    exit 1
}
