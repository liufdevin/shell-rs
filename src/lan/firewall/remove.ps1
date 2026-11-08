try {
    $owned = @()
    foreach ($suffix in @('TCP', 'UDP')) {
        $name = "$rulePrefix-$suffix"
        $existing = @(Get-NetFirewallRule -PolicyStore PersistentStore -Name $name -ErrorAction SilentlyContinue)
        if ($existing.Count -eq 0) { continue }
        if ($existing.Count -ne 1 -or $existing[0].Group -ne $group) { throw 'A different firewall rule uses the reserved name.' }
        $application = $existing[0] | Get-NetFirewallApplicationFilter -ErrorAction Stop
        if ($application.Program -ine $program) { throw 'The reserved firewall rule belongs to a different program.' }
        $owned += $existing[0]
    }
    foreach ($rule in $owned) {
        $rule | Remove-NetFirewallRule -Confirm:$false -ErrorAction Stop
    }
    exit 0
} catch {
    [Console]::Error.Write($_.Exception.Message)
    exit 1
}
