use std::{borrow::Cow, collections::HashMap};

use anyhow::{Context as _, Result, ensure};
use gpui_kit::component::ThemeMode;
use sha2::{Digest as _, Sha256};

use super::TerminalTheme;

const ANSI_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

pub(crate) fn parse_schemes(text: &str) -> Result<Vec<TerminalTheme>> {
    let mut section = String::new();
    let mut colors = HashMap::new();
    let mut named = Vec::new();
    let mut names = std::collections::HashSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = name.to_ascii_lowercase();
            continue;
        }
        if !matches!(section.as_str(), "colors" | "colorthemes") {
            continue;
        }
        let (name, value) = line
            .split_once('=')
            .context("Invalid MobaXterm color entry")?;
        let name = name.trim();
        let value = value.trim();
        if section == "colors" {
            let key = name.to_ascii_lowercase();
            if !matches!(
                key.as_str(),
                "foregroundcolour" | "backgroundcolour" | "cursorcolour"
            ) && !ANSI_NAMES.contains(&key.as_str())
                && !key
                    .strip_prefix("bold")
                    .is_some_and(|key| ANSI_NAMES.contains(&key))
            {
                continue;
            }
            ensure!(
                colors.insert(key, value.to_owned()).is_none(),
                "Duplicate MobaXterm color"
            );
        } else {
            ensure!(
                !name.is_empty()
                    && name.chars().count() <= 200
                    && !name.chars().any(char::is_control),
                "Invalid color scheme name"
            );
            ensure!(
                named.len() < 256 && names.insert(name.to_owned()),
                "Too many or duplicate color schemes"
            );
            let values = value
                .split(',')
                .take(20)
                .map(|color| {
                    let color = color.trim();
                    ensure!(
                        color.len() == 6 && color.bytes().all(|byte| byte.is_ascii_hexdigit()),
                        "Invalid MobaXterm RGB color"
                    );
                    Ok(u32::from_str_radix(color, 16)?)
                })
                .collect::<Result<Vec<_>>>()?;
            ensure!(values.len() == 19, "MobaXterm schemes require 19 colors");
            let mut ansi = [0; 16];
            for index in 0..8 {
                ansi[index] = values[index * 2];
                ansi[index + 8] = values[index * 2 + 1];
            }
            named.push(theme(name, ansi, values[16], values[17], values[18], false));
        }
    }
    if !colors.is_empty() {
        let color = |key: &str| -> Result<u32> {
            let value = colors
                .get(key)
                .with_context(|| format!("Missing MobaXterm color: {key}"))?;
            let components = value
                .split(',')
                .take(4)
                .map(|part| part.trim().parse::<u8>())
                .collect::<std::result::Result<Vec<_>, _>>()?;
            ensure!(
                components.len() == 3,
                "MobaXterm RGB colors require three components"
            );
            Ok((u32::from(components[0]) << 16)
                | (u32::from(components[1]) << 8)
                | u32::from(components[2]))
        };
        let mut ansi = [0; 16];
        for (index, name) in ANSI_NAMES.iter().enumerate() {
            ansi[index] = color(name)?;
            ansi[index + 8] = color(&format!("bold{name}"))?;
        }
        named.insert(
            0,
            theme(
                "MobaXterm — Current colors",
                ansi,
                color("foregroundcolour")?,
                color("backgroundcolour")?,
                color("cursorcolour")?,
                true,
            ),
        );
    }
    Ok(named)
}

fn theme(
    name: &str,
    ansi: [u32; 16],
    foreground: u32,
    background: u32,
    cursor: u32,
    current: bool,
) -> TerminalTheme {
    let bright = ((background >> 16) & 255) * 299
        + ((background >> 8) & 255) * 587
        + (background & 255) * 114;
    let mode = if bright >= 128_000 {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    TerminalTheme {
        key: Cow::Owned(if current {
            "mobaxterm-current".into()
        } else {
            format!("mobaxterm-{:x}", Sha256::digest(name.as_bytes()))
        }),
        name: Cow::Owned(name.into()),
        mode,
        foreground,
        background,
        cursor,
        selection: TerminalTheme::default_for(mode).selection,
        ansi,
        bold_foreground: None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_schemes;
    use gpui_kit::component::ThemeMode;

    fn named(name: &str) -> String {
        let colors = (1..=19)
            .map(|value| format!("{value:06X}"))
            .collect::<Vec<_>>()
            .join(",");
        format!("[ColorThemes]\n{name}={colors}\n")
    }

    #[test]
    fn named_colors_are_rgb_and_interleaved_normal_bright_pairs() {
        let themes = parse_schemes(&named("中文配色")).unwrap();
        assert_eq!(themes.len(), 1);
        let theme = &themes[0];
        assert_eq!(theme.name(), "中文配色");
        assert_eq!(
            theme.ansi,
            [1, 3, 5, 7, 9, 11, 13, 15, 2, 4, 6, 8, 10, 12, 14, 16]
        );
        assert_eq!(
            (theme.foreground, theme.background, theme.cursor),
            (17, 18, 19)
        );
        assert_eq!(theme.mode(), ThemeMode::Dark);
        assert!(!theme.has_separate_bold_colors());
        assert_eq!(
            parse_schemes(&named("中文配色")).unwrap()[0].key(),
            theme.key()
        );
    }

    #[test]
    fn invalid_or_incomplete_colors_do_not_create_partial_schemes() {
        for text in [
            named("Bad").replace("000013", "GGGGGG"),
            named("Bad").replace(",000013", ""),
            format!("{}Bad={}\n", named("Bad"), "000000,".repeat(19)),
            "[Colors]\nForegroundColour=256,0,0\n".into(),
        ] {
            assert!(parse_schemes(&text).is_err());
        }
        assert!(
            parse_schemes("[Bookmarks]\nentry=not-a-color\n[Passwords]\nsecret=do-not-read\n")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn current_and_named_schemes_keep_exact_colors_and_stable_keys() {
        let source = include_str!("../../../tests/fixtures/mobaxterm/colors.ini");
        let schemes = parse_schemes(source).unwrap();
        assert_eq!(schemes.len(), 3);
        assert_eq!(schemes[0].key(), "mobaxterm-current");
        assert_eq!(
            (
                schemes[0].foreground,
                schemes[0].background,
                schemes[0].cursor
            ),
            (0xececec, 0x1e1e1e, 0xb4b4c0)
        );
        assert_eq!(
            (schemes[0].ansi[1], schemes[0].ansi[9]),
            (0xff6060, 0xff8080)
        );
        assert_eq!(schemes[0].ansi, schemes[1].ansi);
        assert_eq!(schemes[2].mode(), ThemeMode::Light);
        let changed = parse_schemes(
            &source.replace("ForegroundColour=236,236,236", "ForegroundColour=1,2,3"),
        )
        .unwrap();
        assert_eq!(changed[0].key(), schemes[0].key());
        assert_eq!(changed[0].foreground, 0x010203);
        let mut settings = crate::settings::TerminalThemeSettings::default();
        settings.import(schemes.clone());
        settings.choose(&schemes[0]);
        settings.import(changed);
        let restored: crate::settings::TerminalThemeSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.imported.len(), 3);
        assert_eq!(restored.theme(ThemeMode::Dark).foreground, 0x010203);
    }
}
