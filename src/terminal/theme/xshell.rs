use std::{borrow::Cow, collections::HashMap};

use anyhow::{Context as _, Result, bail, ensure};
use gpui_kit::component::ThemeMode;
use sha2::{Digest as _, Sha256};

use super::TerminalTheme;

const ANSI_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

pub fn import_xshell_schemes(bytes: &[u8]) -> Result<Vec<TerminalTheme>> {
    ensure!(bytes.len() <= 1024 * 1024, "The .xcs file exceeds 1 MiB");
    let text = decode(bytes)?;
    let mut sections: Vec<(String, HashMap<String, String>)> = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            let name = name.trim();
            ensure!(
                !name.is_empty(),
                "Empty scheme name at line {}",
                line_index + 1
            );
            ensure!(name.chars().count() <= 200, "Scheme name is too long");
            ensure!(
                !sections.iter().any(|(existing, _)| existing == name),
                "Duplicate section: {name}"
            );
            ensure!(sections.len() < 257, "Too many schemes in one file");
            sections.push((name.to_owned(), HashMap::new()));
        } else {
            let (key, value) = line
                .split_once('=')
                .with_context(|| format!("Invalid .xcs line {}", line_index + 1))?;
            let (_, values) = sections.last_mut().context("Missing .xcs section header")?;
            let key = key.trim().to_ascii_lowercase();
            ensure!(
                values
                    .insert(key.clone(), value.trim().to_owned())
                    .is_none(),
                "Duplicate field: {key}"
            );
        }
    }
    let themes = sections
        .into_iter()
        .filter(|(name, _)| !name.eq_ignore_ascii_case("Names"))
        .map(|(name, values)| {
            scheme(&name, &values).with_context(|| format!("Invalid scheme \"{name}\""))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!themes.is_empty(), "No color schemes in the .xcs file");
    Ok(themes)
}

fn decode(bytes: &[u8]) -> Result<Cow<'_, str>> {
    let (encoding, offset) = if let Some(pair) = encoding_rs::Encoding::for_bom(bytes) {
        pair
    } else if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(Cow::Borrowed(text));
    } else {
        (encoding_rs::GB18030, 0)
    };
    let (text, errors) = encoding.decode_without_bom_handling(&bytes[offset..]);
    ensure!(!errors, "Invalid .xcs text encoding");
    Ok(text)
}

fn scheme(name: &str, values: &HashMap<String, String>) -> Result<TerminalTheme> {
    let color = |key: &str| -> Result<u32> {
        let value = values
            .get(key)
            .with_context(|| format!("Missing color: {key}"))?;
        if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("Invalid RGB color {key}={value}; expected RRGGBB");
        }
        Ok(u32::from_str_radix(value, 16)?)
    };
    let foreground = color("text")?;
    let background = color("background")?;
    let mut ansi = [0; 16];
    for (index, key) in ANSI_NAMES.iter().enumerate() {
        ansi[index] = color(key)?;
        ansi[index + 8] = color(&format!("{key}(bold)"))?;
    }
    let bright = ((background >> 16) & 255) * 299
        + ((background >> 8) & 255) * 587
        + (background & 255) * 114;
    let mode = if bright >= 128_000 {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    let default = TerminalTheme::default_for(mode);
    Ok(TerminalTheme {
        key: Cow::Owned(format!("xshell-{:x}", Sha256::digest(name.as_bytes()))),
        name: Cow::Owned(name.to_owned()),
        mode,
        foreground,
        background,
        cursor: foreground,
        selection: default.selection,
        ansi,
        bold_foreground: Some(if values.contains_key("text(bold)") {
            color("text(bold)")?
        } else {
            foreground
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> String {
        let mut text =
            format!("[{name}]\r\ntext=123456\r\ntext(bold)=abcdef\r\nbackground=f7f7f7\r\n");
        for (index, key) in ANSI_NAMES.iter().enumerate() {
            text.push_str(&format!(
                "{key}={:06x}\r\n{key}(bold)={:06x}\r\n",
                index + 1,
                index + 9
            ));
        }
        text
    }

    #[test]
    fn imports_multiple_named_rgb_palettes_and_metadata() {
        let text = format!(
            "{}{}[Names]\ncount=2\nname0=白天\nname1=夜间\n",
            sample("白天"),
            sample("夜间")
        );
        let themes = import_xshell_schemes(text.as_bytes()).unwrap();
        assert_eq!(themes.len(), 2);
        assert_eq!(themes[0].name(), "白天");
        assert_eq!(themes[0].foreground, 0x123456);
        assert_eq!(themes[0].bold_foreground, Some(0xabcdef));
        assert_eq!(themes[0].background, 0xf7f7f7);
        assert_eq!(themes[0].mode, ThemeMode::Light);
        assert_eq!(
            themes[0].ansi,
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(
            themes[0],
            import_xshell_schemes(sample("白天").as_bytes()).unwrap()[0]
        );
    }

    #[test]
    fn imports_netsarangs_published_xcs_file() {
        let themes = import_xshell_schemes(include_bytes!(
            "../../../tests/fixtures/xshell/3024 Day.xcs"
        ))
        .unwrap();
        let theme = &themes[0];
        assert_eq!(theme.name(), "3024 Day");
        assert_eq!(theme.foreground, 0x4a4543);
        assert_eq!(theme.background, 0xf7f7f7);
        assert_eq!(theme.ansi[1], 0xe8bbd0);
        assert_eq!(theme.ansi[9], 0xdb2d20);
    }

    #[test]
    fn accepts_utf8_bom_utf16_and_legacy_chinese_encoding() {
        let text = sample("中文配色");
        let mut utf8 = vec![0xef, 0xbb, 0xbf];
        utf8.extend(text.as_bytes());
        let mut little = vec![0xff, 0xfe];
        let mut big = vec![0xfe, 0xff];
        for unit in text.encode_utf16() {
            little.extend(unit.to_le_bytes());
            big.extend(unit.to_be_bytes());
        }
        let (legacy, _, errors) = encoding_rs::GB18030.encode(&text);
        assert!(!errors);
        for bytes in [&utf8[..], &little, &big, legacy.as_ref()] {
            assert_eq!(import_xshell_schemes(bytes).unwrap()[0].name(), "中文配色");
        }
    }

    #[test]
    fn rejects_missing_invalid_duplicate_and_oversized_data() {
        for text in [
            String::new(),
            "[Names]\ncount=0".into(),
            sample("a").replace("red=000002", "red=xxxxxx"),
            sample("a").replace("red=000002\r\n", ""),
            format!("{}{}", sample("a"), sample("a")),
            format!("{}text=000000", sample("a")),
        ] {
            assert!(import_xshell_schemes(text.as_bytes()).is_err(), "{text}");
        }
        assert!(import_xshell_schemes(&vec![b' '; 1024 * 1024 + 1]).is_err());
    }
}
