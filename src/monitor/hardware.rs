pub(super) const COMMAND: &str = "echo @@mtop; if command -v mtop >/dev/null 2>&1 && command -v timeout >/dev/null 2>&1; then mtop_output=$(timeout -s INT -k 1 1 mtop -n 1 -d 0.5 -m 2>/dev/null); mtop_status=$?; case $mtop_status in 0|124|130) printf \"%s\\n\" \"$mtop_output\";; esac; fi; echo @@mtop-end; ";

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Bandwidth {
    pub name: String,
    pub value: f64,
    pub unit: &'static str,
}

fn channel(raw: &str) -> Option<String> {
    (!raw.is_empty()
        && raw.len() <= 64
        && raw.as_bytes()[0].is_ascii_alphabetic()
        && raw
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '_' | '-' | '.' | '/')))
    .then(|| raw.to_owned())
}

fn number(raw: &str) -> Option<f64> {
    let value = raw.parse::<f64>().ok()?;
    (value.is_finite() && value >= 0.).then_some(value)
}

fn unit(raw: &str) -> Option<&'static str> {
    match raw.to_ascii_lowercase().as_str() {
        "b" | "b/s" => Some("B/s"),
        "kb" | "kb/s" => Some("KB/s"),
        "mb" | "mb/s" => Some("MB/s"),
        "gb" | "gb/s" => Some("GB/s"),
        "kib" | "kib/s" => Some("KiB/s"),
        "mib" | "mib/s" => Some("MiB/s"),
        "gib" | "gib/s" => Some("GiB/s"),
        _ => None,
    }
}

pub(super) fn parse(output: &str) -> Vec<Bandwidth> {
    let mut values = Vec::new();
    let mut header = Vec::new();
    let mut display_unit = None;
    for line in output.lines() {
        let lower = line.to_ascii_lowercase();
        if [
            "error:", "error ", "usage:", "cannot ", "can't ", "invalid ",
        ]
        .iter()
        .any(|word| lower.trim_start().starts_with(word))
            || [
                " failed:",
                " failed!",
                " fail!",
                "permission denied",
                "not support",
            ]
            .iter()
            .any(|word| lower.contains(word))
            || lower.trim_end().ends_with(" failed")
        {
            return Vec::new();
        }
        if let Some(rest) = lower.trim().strip_prefix("unit:") {
            let Some(parsed) = unit(rest.trim()) else {
                return Vec::new();
            };
            display_unit = Some(parsed);
            continue;
        }
        let fields: Vec<_> = line
            .split(|value: char| value.is_whitespace() || matches!(value, '|' | ':' | '='))
            .filter(|field| !field.is_empty())
            .collect();
        if fields.len() == 3
            && let (Some(name), Some(unit)) = (channel(fields[0]), unit(fields[2]))
        {
            values.retain(|item: &Bandwidth| item.name != name);
            if let Some(value) = number(fields[1]) {
                values.push(Bandwidth { name, value, unit });
            }
            continue;
        }
        if !header.is_empty()
            && !fields.is_empty()
            && fields.len() == header.len()
            && fields.iter().all(|field| {
                matches!(
                    field.to_ascii_lowercase().as_str(),
                    "n/a" | "nan" | "inf" | "-"
                )
            })
        {
            values.clear();
            continue;
        }
        if display_unit.is_some()
            && !fields.is_empty()
            && !line.contains(':')
            && fields.iter().all(|field| channel(field).is_some())
        {
            header = fields.iter().map(|field| channel(field)).collect();
            continue;
        }
        if !header.is_empty()
            && fields.len() == header.len()
            && fields.iter().any(|field| number(field).is_some())
        {
            values = header
                .iter()
                .zip(&fields)
                .filter_map(|(name, raw)| {
                    Some(Bandwidth {
                        name: name.clone()?,
                        value: number(raw)?,
                        unit: display_unit?,
                    })
                })
                .collect();
            let mut seen = std::collections::HashSet::new();
            values.retain(|value| seen.insert(value.name.clone()));
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELPER_A133: &str = include_str!("../../tests/fixtures/monitor/mtop-helper-a133.txt");

    #[test]
    fn parses_the_helper_a133_capture_including_signal_cleanup() {
        let values = parse(HELPER_A133);
        assert_eq!(
            values
                .iter()
                .map(|value| (value.name.as_str(), value.value, value.unit))
                .collect::<Vec<_>>(),
            vec![
                ("total", 153., "MB/s"),
                ("caltot", 152., "MB/s"),
                ("cpu", 15., "MB/s"),
                ("gpu", 0., "MB/s"),
                ("de0", 137., "MB/s"),
                ("de1", 0., "MB/s"),
                ("ve", 0., "MB/s"),
                ("csi", 0., "MB/s"),
                ("isp", 0., "MB/s"),
                ("g2d", 0., "MB/s"),
                ("eink", 0., "MB/s")
            ]
        );
    }

    #[test]
    fn preserves_valid_mtop_samples_after_a_graceful_timeout() {
        for status in [0, 124, 130, 1, 127, 137] {
            let script = format!(
                "mtop() {{ test \"$*\" = \"-n 1 -d 0.5 -m\" || return 2; cat <<\"SHELLLF_SAMPLE\"\n{HELPER_A133}SHELLLF_SAMPLE\n}}; timeout() {{ test \"$1\" = -s && test \"$2\" = INT && test \"$3\" = -k && test \"$4\" = 1 && test \"$5\" = 1 || return 2; shift 5; \"$@\"; return {status}; }}; {COMMAND}"
            );
            let output = {
                let _forks = crate::testing::no_forks();
                std::process::Command::new("sh")
                    .args(["-c", &script])
                    .output()
                    .expect("sh runs")
            };
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let values = parse(&String::from_utf8(output.stdout).unwrap());
            assert_eq!(
                values.len(),
                if matches!(status, 0 | 124 | 130) {
                    11
                } else {
                    0
                },
                "status {status}"
            );
        }
    }

    #[test]
    fn reads_tina_columns_and_keeps_the_last_complete_sample() {
        let values = parse(
            "unit: MB\n total cpu gpuddr de_ddr isp_ddr nna_ddr\n 100 10 2.50 12 0 3\n 200 15 5 24 0 6\n",
        );
        assert_eq!(
            values
                .iter()
                .map(|value| (value.name.as_str(), value.value, value.unit))
                .collect::<Vec<_>>(),
            vec![
                ("total", 200., "MB/s"),
                ("cpu", 15., "MB/s"),
                ("gpuddr", 5., "MB/s"),
                ("de_ddr", 24., "MB/s"),
                ("isp_ddr", 0., "MB/s"),
                ("nna_ddr", 6., "MB/s")
            ]
        );
    }

    #[test]
    fn preserves_distinct_ports_and_explicit_units() {
        let values = parse("GPU: 2.50 MB/s\nNPU: 20 KB/s\nDE0: 5 MiB/s\nDE1: 6 MiB/s\n");
        assert_eq!(values.len(), 4);
        assert_eq!(values[1].unit, "KB/s");
        assert_eq!(values[3].name, "DE1");
        assert_eq!(channel("aipu_ddr"), Some("aipu_ddr".into()));
        assert_eq!(channel("gpuddr1"), Some("gpuddr1".into()));
        assert_eq!(channel("g2d"), Some("g2d".into()));
    }

    #[test]
    fn absent_failed_unknown_and_invalid_metrics_are_not_zero_readings() {
        for output in [
            "",
            "Usage: mtop -n iter",
            "GPU 5 MB/s\nopen failed",
            "unit: cycles\ngpu\n10",
            "GPU -1 MB/s",
            "NPU NaN MB/s",
            "DE inf MB/s",
            "GPU 25 %",
            "gpu npu\nN/A N/A",
            "GPU MHz 600",
        ] {
            assert!(parse(output).is_empty(), "{output}");
        }
        let values = parse("unit: MB\ntotal cpu de_ddr\n10 2 3\n");
        assert_eq!(values.len(), 3);
        assert_eq!(values[2].name, "de_ddr");
        let values = parse("unit: MB\ngpu npu\n1 N/A");
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].name, "gpu");
    }

    #[test]
    fn discovers_vendor_specific_channels_without_a_fixed_device_list() {
        let values = parse("unit: KB\ncpuddr dsp_ddr aipu_ddr pcie0 mahb_ddr\n1 2 3 4 5\n");
        assert_eq!(
            values
                .iter()
                .map(|value| value.name.as_str())
                .collect::<Vec<_>>(),
            ["cpuddr", "dsp_ddr", "aipu_ddr", "pcie0", "mahb_ddr"]
        );
        assert!(values.iter().all(|value| value.unit == "KB/s"));
    }
}
