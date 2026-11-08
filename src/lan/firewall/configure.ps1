try {
    foreach ($entry in @(@{ Suffix = 'TCP'; Protocol = 'TCP'; Port = $tcpPort }, @{ Suffix = 'UDP'; Protocol = 'UDP'; Port = $udpPort })) {
        $name = "$rulePrefix-$($entry.Suffix)"
        $parameters = @{
            PolicyStore = 'PersistentStore'
            Name = $name
            DisplayName = $name
            Group = $group
            Program = $program
            Direction = 'Inbound'
            Action = 'Allow'
            Enabled = 'True'
            Profile = 'Private'
            Protocol = $entry.Protocol
            LocalPort = $entry.Port
            RemotePort = 'Any'
            LocalAddress = 'Any'
            RemoteAddress = 'LocalSubnet'
            Service = 'Any'
            InterfaceAlias = 'Any'
            InterfaceType = 'Any'
            EdgeTraversalPolicy = 'Block'
            Authentication = 'NotRequired'
            Encryption = 'NotRequired'
            OverrideBlockRules = $false
        }
        $existing = @(Get-NetFirewallRule -PolicyStore PersistentStore -Name $name -ErrorAction SilentlyContinue)
        if ($existing.Count -gt 0) {
            if ($existing.Count -ne 1 -or $existing[0].Group -ne $group) { throw 'A different firewall rule uses the reserved name.' }
            $application = $existing[0] | Get-NetFirewallApplicationFilter -ErrorAction Stop
            if ($application.Program -ine $program) { throw 'The reserved firewall rule belongs to a different program.' }
            $parameters.NewDisplayName = $parameters.DisplayName
            $parameters.Remove('DisplayName')
            $parameters.Remove('Group')
            Set-NetFirewallRule @parameters -ErrorAction Stop | Out-Null
        } else {
            New-NetFirewallRule @parameters -ErrorAction Stop | Out-Null
        }
    }
    exit 0
} catch {
    [Console]::Error.Write($_.Exception.Message)
    exit 1
}
