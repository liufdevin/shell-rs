use std::{
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use base64::Engine as _;
use sha2::{Digest as _, Sha256};

use super::{FirewallFailure, FirewallReport};

const GROUP: &str = "ShellLF LAN cooperation";
const CHECK_SCRIPT: &str = include_str!("check.ps1");
const CONFIGURE_SCRIPT: &str = include_str!("configure.ps1");
const REMOVE_SCRIPT: &str = include_str!("remove.ps1");

fn failure(error: impl std::fmt::Display) -> FirewallFailure {
    FirewallFailure::Failed(
        error
            .to_string()
            .chars()
            .filter(|character| !character.is_control())
            .take(1200)
            .collect(),
    )
}

fn program() -> Result<PathBuf, FirewallFailure> {
    dunce::canonicalize(std::env::current_exe().map_err(failure)?).map_err(failure)
}

fn powershell() -> Result<PathBuf, FirewallFailure> {
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buffer = vec![0u16; 32768];
    let count = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if count == 0 || count >= buffer.len() {
        return Err(failure(std::io::Error::last_os_error()));
    }
    use std::os::windows::ffi::OsStringExt as _;
    let executable = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..count]))
        .join("WindowsPowerShell/v1.0/powershell.exe");
    if !executable.is_file() {
        return Err(failure("Windows PowerShell is unavailable"));
    }
    Ok(executable)
}

fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn script(program: &Path, body: &str) -> Result<String, FirewallFailure> {
    let value = program
        .to_str()
        .filter(|value| !value.chars().any(char::is_control))
        .ok_or_else(|| failure("invalid program path"))?;
    if !program.is_absolute() {
        return Err(failure("program path must be absolute"));
    }
    let hash = Sha256::digest(value.to_lowercase().as_bytes());
    let key: String = hash[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!(
        "$ErrorActionPreference = 'Stop'\n$ProgressPreference = 'SilentlyContinue'\n[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)\n$env:PSModulePath = Join-Path $PSHOME 'Modules'\n{}\n$program = {}\n$group = {}\n$rulePrefix = 'ShellLF-LAN-{key}'\n$tcpPort = {}\n$udpPort = {}\n{}",
        if body != CHECK_SCRIPT {
            "Import-Module (Join-Path $PSHOME 'Modules/NetSecurity/NetSecurity.psd1') -ErrorAction Stop"
        } else {
            ""
        },
        quoted(value),
        quoted(GROUP),
        shellrs_lan::TCP_PORT,
        shellrs_lan::DISCOVERY_PORT,
        body
    ))
}

fn encoded(script: &str) -> String {
    let bytes: Vec<_> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub(super) fn check() -> Result<FirewallReport, FirewallFailure> {
    let script = script(&program()?, CHECK_SCRIPT)?;
    serde_json::from_slice(&run_query(&script)?).map_err(failure)
}

fn run_query(script: &str) -> Result<Vec<u8>, FirewallFailure> {
    let log = tempfile::tempfile().map_err(failure)?;
    let errors = tempfile::tempfile().map_err(failure)?;
    let mut command = Command::new(powershell()?);
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded(script),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(failure)?))
        .stderr(Stdio::from(errors.try_clone().map_err(failure)?));
    use std::os::windows::process::CommandExt as _;
    command.creation_flags(0x0800_0000);
    let mut child = command.spawn().map_err(failure)?;
    let start = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if start.elapsed() < Duration::from_secs(45) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Ok(None) => break Err(FirewallFailure::TimedOut),
            Err(error) => break Err(failure(error)),
        }
    };
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let status = result?;
    let mut log = log;
    if log.metadata().map_err(failure)?.len() > 65536 {
        return Err(failure("firewall response too large"));
    }
    log.seek(SeekFrom::Start(0)).map_err(failure)?;
    let mut bytes = Vec::new();
    log.take(65536).read_to_end(&mut bytes).map_err(failure)?;
    if !status.success() {
        let mut errors = errors;
        errors.seek(SeekFrom::Start(0)).map_err(failure)?;
        let mut detail = Vec::new();
        errors
            .take(4096)
            .read_to_end(&mut detail)
            .map_err(failure)?;
        return Err(failure(format!(
            "Windows firewall query failed ({status}): {}",
            String::from_utf8_lossy(&detail)
        )));
    }
    Ok(bytes)
}

pub(super) fn configure() -> Result<(), FirewallFailure> {
    elevate(&script(&program()?, CONFIGURE_SCRIPT)?)
}

pub(super) fn remove() -> Result<(), FirewallFailure> {
    elevate(&script(&program()?, REMOVE_SCRIPT)?)
}

fn elevate(script: &str) -> Result<(), FirewallFailure> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_CANCELLED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::{
            Com::{
                COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
            },
            Threading::{GetExitCodeProcess, WaitForSingleObject},
        },
        UI::{
            Shell::{
                SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
                ShellExecuteExW,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    };
    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    let initialized = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    if initialized < 0 {
        return Err(failure(format!(
            "COM initialization failed: {initialized:#x}"
        )));
    }
    let _com = Com;
    let executable = powershell()?;
    let wide = |value: &std::ffi::OsStr| {
        value
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let verb = wide(std::ffi::OsStr::new("runas"));
    let file = wide(executable.as_os_str());
    let parameters = wide(std::ffi::OsStr::new(&format!(
        "-NoLogo -NoProfile -NonInteractive -EncodedCommand {}",
        encoded(script)
    )));
    let directory = wide(executable.parent().unwrap().as_os_str());
    let mut information = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        lpDirectory: directory.as_ptr(),
        nShow: SW_HIDE,
        ..unsafe { std::mem::zeroed() }
    };
    if unsafe { ShellExecuteExW(&mut information) } == 0 {
        let error = std::io::Error::last_os_error();
        return Err(if error.raw_os_error() == Some(ERROR_CANCELLED as i32) {
            FirewallFailure::Cancelled
        } else {
            failure(error)
        });
    }
    if information.hProcess.is_null() {
        return Err(failure(
            "elevated firewall task did not return a process handle",
        ));
    }
    struct Process(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for Process {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
    let process = Process(information.hProcess);
    match unsafe { WaitForSingleObject(process.0, 120000) } {
        WAIT_OBJECT_0 => {}
        WAIT_TIMEOUT => return Err(FirewallFailure::TimedOut),
        _ => return Err(failure(std::io::Error::last_os_error())),
    }
    let mut code = 0;
    if unsafe { GetExitCodeProcess(process.0, &mut code) } == 0 {
        return Err(failure(std::io::Error::last_os_error()));
    }
    if code != 0 {
        return Err(failure(
            "Windows did not complete the firewall operation; check administrator rights and system policy",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_powershell_executes_a_bounded_hidden_query() {
        assert_eq!(
            run_query("[Console]::Out.Write('ok'); exit 0").unwrap(),
            b"ok"
        );
    }

    #[test]
    fn scripts_quote_paths_and_bind_rules_to_one_program_and_private_ports() {
        let program = Path::new("C:\\tools\\Shell LF 'demo' $name;测试\\ShellLF.exe");
        let configure = script(program, CONFIGURE_SCRIPT).unwrap();
        assert!(
            configure.contains("$program = 'C:\\tools\\Shell LF ''demo'' $name;测试\\ShellLF.exe'")
        );
        assert!(
            configure.contains("Profile = 'Private'")
                && configure.contains("RemoteAddress = 'LocalSubnet'")
        );
        assert!(
            configure.contains("Program = $program")
                && configure.contains("$tcpPort = 45838")
                && configure.contains("$udpPort = 45837")
        );
        assert!(
            !configure.contains("Set-NetFirewallProfile")
                && !configure.contains("Set-NetConnectionProfile")
        );
        assert!(!configure.contains("Remove-NetFirewallRule") && !configure.contains("Public"));
        let check = script(program, CHECK_SCRIPT).unwrap();
        for verb in [
            "New-NetFirewallRule",
            "Set-NetFirewallRule",
            "Remove-NetFirewallRule",
            "Start-Process",
        ] {
            assert!(!check.contains(verb));
        }
        assert!(check.contains("HNetCfg.FwPolicy2") && check.contains("LocalPolicyModifyState"));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded(&configure))
            .unwrap();
        let characters: Vec<_> = decoded
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| u16::from_le_bytes(*bytes))
            .collect();
        assert_eq!(String::from_utf16(&characters).unwrap(), configure);
        assert!(script(Path::new("relative.exe"), CONFIGURE_SCRIPT).is_err());
        assert!(script(Path::new("C:\\bad\npath.exe"), CONFIGURE_SCRIPT).is_err());
    }

    #[test]
    #[ignore = "reads real Windows firewall rules without changing any system settings"]
    fn real_windows_firewall_query_is_read_only_and_returns_typed_results() {
        let report = check().unwrap();
        assert!(report.possible_blocks < 100000);
    }

    const MOCK_HEADER: &str = r#"
$ErrorActionPreference = 'Stop'
$program = 'C:\ShellLF\ShellLF.exe'
$group = 'ShellLF LAN cooperation'
$rulePrefix = 'ShellLF-LAN-test'
$tcpPort = 45838
$udpPort = 45837
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
"#;

    const MOCK_QUERY: &str = r#"
function New-Object {
    [CmdletBinding()] param($ComObject)
    if ($ComObject -ne 'HNetCfg.FwPolicy2' -or $case -eq 'failure') { throw 'query failed' }
    $rules = @()
    foreach ($protocol in @(6, 17)) {
        $tcp = $protocol -eq 6
        if ($case -eq 'missing' -and $tcp) { continue }
        $rules += [pscustomobject]@{
            Name = $(if ($tcp) { "$rulePrefix-TCP" } else { "$rulePrefix-UDP" })
            Enabled = $true; Direction = 1; Action = 1; Profiles = $(if ($case -eq 'profile') { 6 } else { 2 })
            EdgeTraversal = $false; EdgeTraversalOptions = 0; Grouping = $group
            ApplicationName = $(if ($case -eq 'program') { '*' } else { $program })
            Protocol = $protocol; LocalPorts = $(if ($case -eq 'port') { '*' } elseif ($tcp) { '45838' } else { '45837' })
            RemotePorts = '*'; LocalAddresses = '*'; RemoteAddresses = $(if ($case -eq 'remote') { '*' } else { 'LocalSubnet' })
            ServiceName = $(if ($case -eq 'service') { 'other-service' } else { '' })
            Interfaces = $(if ($case -eq 'interface') { @('other-interface') } else { @() }); InterfaceTypes = 'All'
            SecureFlags = $(if ($case -eq 'security') { 1 } else { 0 }); LocalAppPackageId = ''; LocalUserAuthorizedList = ''; RemoteUserAuthorizedList = ''; RemoteMachineAuthorizedList = ''
        }
    }
    if ($case -eq 'blocked') { $rules += [pscustomobject]@{ Name = 'block'; Enabled = $true; Profiles = 2147483647; Direction = 1; Action = 0; ApplicationName = '*'; Protocol = 256; LocalPorts = '*' } }
    $policy = [pscustomobject]@{ Rules = $rules; CurrentProfileTypes = $(if ($case -eq 'public') { 4 } else { 2 }); LocalPolicyModifyState = $(if ($case -eq 'managed') { 1 } else { 0 }); Disabled = $case -eq 'disabled' }
    $policy | Add-Member -MemberType ScriptMethod -Name FirewallEnabled -Value { param($profile) return -not $this.Disabled }
    $policy | Add-Member -MemberType ScriptMethod -Name BlockAllInboundTraffic -Value { param($profile) return $false }
    return $policy
}
"#;

    #[test]
    fn checker_evaluates_real_powershell_filter_logic_without_accessing_the_firewall() {
        for case in [
            "ready",
            "missing",
            "program",
            "profile",
            "port",
            "remote",
            "service",
            "interface",
            "security",
            "public",
            "disabled",
            "managed",
            "blocked",
        ] {
            let script = format!(
                "{MOCK_HEADER}\n$case = {}\n{MOCK_QUERY}\n{CHECK_SCRIPT}",
                quoted(case)
            );
            let bytes = run_query(&script).unwrap_or_else(|error| panic!("{case}: {error:?}"));
            let report: FirewallReport = serde_json::from_slice(&bytes).unwrap();
            let rules_ready = ![
                "missing",
                "program",
                "profile",
                "port",
                "remote",
                "service",
                "interface",
                "security",
            ]
            .contains(&case);
            assert_eq!(report.tcp_rule && report.udp_rule, rules_ready, "{case}");
            assert_eq!(report.private_network, case != "public");
            assert_eq!(report.firewall_enabled, case != "disabled");
            assert_eq!(report.local_rules_allowed, case != "managed");
            assert_eq!(report.possible_blocks, u32::from(case == "blocked"));
            assert_eq!(report.managed_rules, if case == "missing" { 1 } else { 2 });
        }
        assert!(
            run_query(&format!(
                "{MOCK_HEADER}\n$case = 'failure'\n{MOCK_QUERY}\n{CHECK_SCRIPT}"
            ))
            .is_err()
        );
    }

    const MOCK_CONFIGURE: &str = r#"
function Get-NetFirewallRule {
    [CmdletBinding()] param($PolicyStore, $Name)
    if ($case -ne 'new') { [pscustomobject]@{ Name = $Name; Group = $(if ($case -eq 'collision' -or ($case -eq 'second-collision' -and $Name -like '*-UDP')) { 'Other application' } else { $group }) } }
}
function Get-NetFirewallApplicationFilter {
    [CmdletBinding()] param([Parameter(ValueFromPipeline)]$rule)
    process { [pscustomobject]@{ Program = $(if ($case -eq 'collision-program') { 'C:\Other\other.exe' } else { $program }) } }
}
function Assert-RuleParameters($parameters) {
    if ($parameters.Profile -ne 'Private' -or $parameters.Program -ne $program -or $parameters.RemoteAddress -ne 'LocalSubnet' -or $parameters.PolicyStore -ne 'PersistentStore') { throw 'unsafe rule scope' }
    if ($parameters.Direction -ne 'Inbound' -or $parameters.Action -ne 'Allow' -or $parameters.Enabled -ne 'True' -or $parameters.EdgeTraversalPolicy -ne 'Block') { throw 'incorrect rule action' }
    if (($parameters.Protocol -eq 'TCP' -and $parameters.LocalPort -ne 45838) -or ($parameters.Protocol -eq 'UDP' -and $parameters.LocalPort -ne 45837)) { throw 'incorrect port' }
    if ($parameters.Authentication -ne 'NotRequired' -or $parameters.Encryption -ne 'NotRequired' -or $parameters.OverrideBlockRules) { throw 'incorrect security' }
}
function New-NetFirewallRule {
    [CmdletBinding()] param($Name, $DisplayName, $Group, $PolicyStore, $Program, $Direction, $Action, $Enabled, $Profile, $Protocol, $LocalPort, $RemotePort, $LocalAddress, $RemoteAddress, $Service, $InterfaceAlias, $InterfaceType, $EdgeTraversalPolicy, $Authentication, $Encryption, $OverrideBlockRules)
    Assert-RuleParameters $PSBoundParameters
    [Console]::Out.Write('new;')
}
function Set-NetFirewallRule {
    [CmdletBinding()] param($Name, $NewDisplayName, $PolicyStore, $Program, $Direction, $Action, $Enabled, $Profile, $Protocol, $LocalPort, $RemotePort, $LocalAddress, $RemoteAddress, $Service, $InterfaceAlias, $InterfaceType, $EdgeTraversalPolicy, $Authentication, $Encryption, $OverrideBlockRules)
    Assert-RuleParameters $PSBoundParameters
    [Console]::Out.Write('set;')
}
"#;

    #[test]
    fn configuration_creates_or_repairs_only_owned_private_rules_using_mock_cmdlets() {
        for (case, expected) in [("new", "new;new;"), ("existing", "set;set;")] {
            let output = run_query(&format!(
                "{MOCK_HEADER}\n$case = {}\n{MOCK_CONFIGURE}\n{CONFIGURE_SCRIPT}",
                quoted(case)
            ))
            .unwrap();
            assert_eq!(String::from_utf8(output).unwrap(), expected);
        }
        assert!(
            run_query(&format!(
                "{MOCK_HEADER}\n$case = 'collision'\n{MOCK_CONFIGURE}\n{CONFIGURE_SCRIPT}"
            ))
            .is_err()
        );
        assert!(
            run_query(&format!(
                "{MOCK_HEADER}\n$case = 'collision-program'\n{MOCK_CONFIGURE}\n{CONFIGURE_SCRIPT}"
            ))
            .is_err()
        );
    }

    #[test]
    fn removal_uses_only_validated_rule_objects_and_is_safe_when_already_absent() {
        let removal = r#"
function Remove-NetFirewallRule {
    [CmdletBinding(SupportsShouldProcess)] param([Parameter(ValueFromPipeline)]$InputObject)
    process {
        if ($case -eq 'second-collision') { throw 'unsafe deletion before all ownership checks' }
        if ($InputObject.Name -notin @("$rulePrefix-TCP", "$rulePrefix-UDP") -or $InputObject.Group -ne $group) { throw 'unsafe rule removal' }
        [Console]::Out.Write('removed;')
    }
}
"#;
        for (case, expected) in [("existing", "removed;removed;"), ("new", "")] {
            let output = run_query(&format!(
                "{MOCK_HEADER}\n$case = {}\n{MOCK_CONFIGURE}\n{removal}\n{REMOVE_SCRIPT}",
                quoted(case)
            ))
            .unwrap();
            assert_eq!(String::from_utf8(output).unwrap(), expected);
        }
        for case in ["collision", "collision-program"] {
            assert!(
                run_query(&format!(
                    "{MOCK_HEADER}\n$case = {}\n{MOCK_CONFIGURE}\n{removal}\n{REMOVE_SCRIPT}",
                    quoted(case)
                ))
                .is_err()
            );
        }
        let result = run_query(&format!(
            "{MOCK_HEADER}\n$case = 'second-collision'\n{MOCK_CONFIGURE}\n{removal}\n{REMOVE_SCRIPT}"
        ));
        match result {
            Err(FirewallFailure::Failed(reason)) => {
                assert!(reason.contains("A different firewall rule uses the reserved name"))
            }
            other => panic!("unexpected preflight result: {other:?}"),
        }
        let remove = script(Path::new("C:\\ShellLF\\ShellLF.exe"), REMOVE_SCRIPT).unwrap();
        assert!(
            remove.contains("PolicyStore PersistentStore")
                && remove.contains("$application.Program -ine $program")
        );
        assert!(
            !remove.contains("Set-NetFirewallProfile") && !remove.contains("New-NetFirewallRule")
        );
    }

    #[test]
    fn in_memory_native_windows_rules_are_readable_without_installing_them() {
        let prepare = r#"
$nativeRules = @()
foreach ($protocol in @(6, 17)) {
    $rule = New-Object -ComObject HNetCfg.FWRule
    $rule.Name = $(if ($protocol -eq 6) { "$rulePrefix-TCP" } else { "$rulePrefix-UDP" })
    $rule.ApplicationName = $program
    $rule.Grouping = $group
    $rule.Protocol = $protocol
    $rule.LocalPorts = $(if ($protocol -eq 6) { '45838' } else { '45837' })
    $rule.RemoteAddresses = 'LocalSubnet'
    $rule.Profiles = 2
    $rule.Direction = 1
    $rule.Action = 1
    $rule.Enabled = $true
    $nativeRules += $rule
}
$policy = [pscustomobject]@{ Rules = $nativeRules; CurrentProfileTypes = 2; LocalPolicyModifyState = 0 }
$policy | Add-Member -MemberType ScriptMethod -Name FirewallEnabled -Value { param($profile) return $true }
$policy | Add-Member -MemberType ScriptMethod -Name BlockAllInboundTraffic -Value { param($profile) return $false }
function New-Object { [CmdletBinding()] param($ComObject); return $policy }
"#;
        let output = run_query(&format!("{MOCK_HEADER}\n{prepare}\n{CHECK_SCRIPT}")).unwrap();
        let report: FirewallReport = serde_json::from_slice(&output).unwrap();
        assert!(report.tcp_rule && report.udp_rule);
    }
}
