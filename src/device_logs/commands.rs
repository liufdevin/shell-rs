#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    Android,
    Kernel,
    Journal,
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Privilege {
    User,
    CurrentRoot,
    Su,
    SuZero,
    Sudo,
}

pub(super) const PROBE: &str = "if command -v getprop >/dev/null 2>&1 && test -n \"$(getprop ro.build.version.sdk 2>/dev/null)\" && command -v logcat >/dev/null 2>&1; then printf '__SHELLLF_LOG_ANDROID__\\n'; elif test \"$(uname -s 2>/dev/null)\" = Linux && command -v dmesg >/dev/null 2>&1; then printf '__SHELLLF_LOG_KERNEL__\\n'; else printf '__SHELLLF_LOG_UNSUPPORTED__\\n'; fi";

pub(super) fn source(output: &str) -> Option<Source> {
    output.lines().find_map(|line| match line.trim() {
        "__SHELLLF_LOG_ANDROID__" => Some(Source::Android),
        "__SHELLLF_LOG_KERNEL__" => Some(Source::Kernel),
        _ => None,
    })
}

pub(super) fn privilege_probe(source: Source) -> &'static str {
    match source {
        Source::Android => {
            "if test \"$(id -u)\" = 0; then echo __SHELLLF_LOG_ROOT__; elif command -v su >/dev/null 2>&1 && command -v timeout >/dev/null 2>&1; then if test \"$(timeout 3 su -c 'id -u' </dev/null 2>/dev/null)\" = 0; then echo __SHELLLF_LOG_SU__; elif test \"$(timeout 3 su 0 id -u </dev/null 2>/dev/null)\" = 0; then echo __SHELLLF_LOG_SU_ZERO__; else echo __SHELLLF_LOG_DENIED__; fi; else echo __SHELLLF_LOG_DENIED__; fi"
        }
        Source::Kernel | Source::Journal | Source::File => {
            "if test \"$(id -u)\" = 0; then echo __SHELLLF_LOG_ROOT__; elif command -v sudo >/dev/null 2>&1 && sudo -n -- dmesg -r </dev/null >/dev/null 2>&1; then echo __SHELLLF_LOG_SUDO__; else echo __SHELLLF_LOG_DENIED__; fi"
        }
    }
}

pub(super) fn privilege(output: &str) -> Option<Privilege> {
    output.lines().find_map(|line| match line.trim() {
        "__SHELLLF_LOG_USER__" => Some(Privilege::User),
        "__SHELLLF_LOG_ROOT__" => Some(Privilege::CurrentRoot),
        "__SHELLLF_LOG_SU__" => Some(Privilege::Su),
        "__SHELLLF_LOG_SU_ZERO__" => Some(Privilege::SuZero),
        "__SHELLLF_LOG_SUDO__" => Some(Privilege::Sudo),
        _ => None,
    })
}

#[cfg(test)]
pub(super) fn read_command(source: Source, privilege: Privilege) -> String {
    query_command(source, privilege, &super::LogOptions::default())
}

pub(super) fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

pub(super) fn effective_source(platform: Source, options: &super::LogOptions) -> Source {
    if platform == Source::Android {
        platform
    } else {
        match options.source {
            super::LogSource::Device => Source::Kernel,
            super::LogSource::System | super::LogSource::Service => Source::Journal,
            super::LogSource::File => Source::File,
        }
    }
}

pub(super) fn query_command(
    source: Source,
    privilege: Privilege,
    options: &super::LogOptions,
) -> String {
    let command = match source {
        Source::Android => {
            let buffers = if options.crashes {
                " -b crash"
            } else if privilege != Privilege::User {
                " -b all"
            } else {
                ""
            };
            format!("logcat -d{buffers} -v threadtime -t 2000")
        }
        Source::Kernel => "dmesg -r".into(),
        Source::Journal => {
            let unit = if options.source == super::LogSource::Service {
                format!(" --unit={}", quote(&options.object))
            } else {
                String::new()
            };
            let period = match options.period {
                super::LogPeriod::PreviousBoot => " -b -1".into(),
                _ => match options.period.seconds() {
                    Some(seconds) => format!(" -b 0 --since='-{seconds} seconds'"),
                    None => " -b 0".into(),
                },
            };
            format!("journalctl --no-pager --quiet --output=json -n 2000{unit}{period}")
        }
        Source::File => format!("tail -n 2000 -- {}", quote(&options.object)),
    };
    let command = if privilege == Privilege::Sudo {
        format!("sudo -n -- {command} </dev/null")
    } else {
        command
    };
    let metadata = if source == Source::Android && !options.object.is_empty() {
        format!(
            "pids=$(pidof {} 2>/dev/null || :); printf '__SHELLLF_LOG_PIDS__ %s\\n' \"$pids\";",
            quote(&options.object)
        )
    } else {
        String::new()
    };
    let check = if source == Source::Android && !options.object.is_empty() {
        "if ! command -v pidof >/dev/null 2>&1; then printf '__SHELLLF_LOG_ERROR__\\npidof is unavailable\\n'; exit 0; fi;"
    } else {
        ""
    };
    let script = format!(
        "{check} output=$({command} 2>&1); code=$?; if test \"$code\" = 0; then printf '__SHELLLF_LOG_OK__\\n'; date '+__SHELLLF_LOG_CLOCK__ %Y-%m-%dT%H:%M:%S'; {metadata} printf '%s\\n' \"$output\" | tail -n 2000; else printf '__SHELLLF_LOG_ERROR__\\n%s\\n' \"$output\"; fi"
    );
    match privilege {
        Privilege::User | Privilege::CurrentRoot => format!("sh -c {}", quote(&script)),
        Privilege::Su => format!("timeout 8 su -c {} </dev/null", quote(&script)),
        Privilege::SuZero => format!("timeout 8 su 0 sh -c {} </dev/null", quote(&script)),
        Privilege::Sudo => format!("sh -c {}", quote(&script)),
    }
}

pub(super) fn access_probe(source: Source, options: &super::LogOptions) -> String {
    if source == Source::Android || source == Source::Kernel {
        return privilege_probe(source).into();
    }
    let command = if source == Source::Journal {
        "journalctl --no-pager --quiet -n 1".into()
    } else {
        format!("tail -n 1 -- {}", quote(&options.object))
    };
    format!(
        "if test \"$(id -u)\" = 0; then echo __SHELLLF_LOG_ROOT__; elif command -v sudo >/dev/null 2>&1 && sudo -n -- {command} </dev/null >/dev/null 2>&1; then echo __SHELLLF_LOG_SUDO__; else echo __SHELLLF_LOG_DENIED__; fi"
    )
}

pub(super) fn read_output(output: &str) -> Result<&str, String> {
    let result = output
        .strip_prefix("__SHELLLF_LOG_OK__\r\n")
        .or_else(|| output.strip_prefix("__SHELLLF_LOG_OK__\n"));
    result.ok_or_else(|| {
        output
            .lines()
            .filter(|line| !line.starts_with("__SHELLLF_LOG_"))
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .take(1000)
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_filters_and_file_paths_are_quoted_and_elevation_is_scoped() {
        let options = super::super::LogOptions::service("nginx.service")
            .with_period(super::super::LogPeriod::PreviousBoot);
        let journal = query_command(Source::Journal, Privilege::Sudo, &options);
        assert!(journal.contains("sudo -n -- journalctl"));
        assert!(!journal.contains("sudo -n -- sh"));
        assert!(journal.contains("-b -1"));
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("a'; printf injected; echo '.log");
        std::fs::write(&path, "literal file contents").unwrap();
        let options = super::super::LogOptions::file(path.to_string_lossy().replace('\\', "/"));
        let command = query_command(Source::File, Privilege::User, &options);
        let _forks = crate::testing::no_forks();
        let output = std::process::Command::new("sh")
            .args(["-c", &command])
            .output()
            .unwrap();
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(output.contains("literal file contents"), "{output}");
        assert!(!output.contains("injected"));
    }

    #[test]
    fn application_pids_are_resolved_on_each_read_without_shell_interpolation() {
        let options = super::super::LogOptions::application("com.example.demo");
        let command = query_command(Source::Android, Privilege::User, &options);
        assert!(command.contains("pidof"));
        assert!(command.contains("__SHELLLF_LOG_PIDS__"));
        assert!(!command.contains("--pid="));
        let options = super::super::LogOptions::service("demo.service")
            .with_period(super::super::LogPeriod::Minutes10);
        let command = query_command(Source::Journal, Privilege::User, &options);
        assert!(command.contains("-600 seconds"));
    }

    #[test]
    fn commands_never_clear_device_buffers_or_wait_for_a_password() {
        for source in [Source::Android, Source::Kernel] {
            for privilege in [
                Privilege::User,
                Privilege::CurrentRoot,
                Privilege::Su,
                Privilege::SuZero,
                Privilege::Sudo,
            ] {
                let command = read_command(source, privilege);
                assert!(!command.contains("logcat -c") && !command.contains("dmesg -c"));
                assert!(command.contains("tail -n 2000"));
            }
        }
        assert!(privilege_probe(Source::Android).contains("timeout 3"));
        assert!(privilege_probe(Source::Kernel).contains("sudo -n"));
        assert_eq!(privilege("__SHELLLF_LOG_DENIED__"), None);
        assert!(!read_command(Source::Android, Privilege::User).contains("-b all"));
        assert!(read_command(Source::Android, Privilege::CurrentRoot).contains("-b all"));
        assert_eq!(
            privilege("__SHELLLF_LOG_SU_ZERO__\n"),
            Some(Privilege::SuZero)
        );
        assert!(
            read_command(Source::Android, Privilege::SuZero).starts_with("timeout 8 su 0 sh -c")
        );
        assert!(read_output("__SHELLLF_LOG_ERROR__\nPermission denied\n").is_err());
    }

    #[test]
    #[ignore = "reads logs from an explicitly selected connected ADB device without elevation"]
    fn connected_device_can_read_logs_without_modifying_buffers() {
        let program = std::env::var_os("SHELLRS_TEST_ADB").expect("SHELLRS_TEST_ADB is required");
        let serial =
            std::env::var("SHELLRS_TEST_ADB_SERIAL").expect("SHELLRS_TEST_ADB_SERIAL is required");
        let device = crate::adb::AdbConnection::new(program.into(), serial).unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let detected = device.monitor_command(PROBE, &cancel).unwrap();
        let source = source(&detected).expect("a supported source");
        let output = device
            .monitor_command(&read_command(source, Privilege::User), &cancel)
            .unwrap();
        let text = read_output(&output).expect("current user can read logs");
        assert!(text.lines().count() <= 2000);
        println!(
            "{source:?}: {} lines read, no device buffer was cleared",
            text.lines().count()
        );
    }
}
