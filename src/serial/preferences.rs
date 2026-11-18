use serde::{Deserialize, Serialize};

use crate::host::{DataBits, Parity, SerialSettings, StopBits, TerminalEncoding};

use super::{SerialLineEnding, SerialToolPage};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct SerialAssistantPreferences {
    pub(crate) port: Option<String>,
    pub(crate) baud: Option<u32>,
    pub(crate) custom_baud: String,
    pub(crate) parity: Parity,
    pub(crate) data_bits: DataBits,
    pub(crate) stop_bits: StopBits,
    pub(crate) encoding: TerminalEncoding,
    pub(crate) receive_hex: bool,
    pub(crate) follow_output: bool,
    pub(crate) sender: SerialSenderPreferences,
    pub(crate) log_filter: Option<SerialLogFilter>,
}

impl Default for SerialAssistantPreferences {
    fn default() -> Self {
        let line = SerialSettings::default();
        Self {
            port: None,
            baud: Some(line.baud),
            custom_baud: line.baud.to_string(),
            parity: line.parity,
            data_bits: line.data_bits,
            stop_bits: line.stop_bits,
            encoding: TerminalEncoding::default(),
            receive_hex: false,
            follow_output: true,
            sender: SerialSenderPreferences::default(),
            log_filter: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SerialSenderPreferences {
    pub(crate) message: String,
    pub(crate) pattern: String,
    pub(crate) reply: String,
    pub(crate) baud: String,
    pub(crate) interval: String,
    pub(crate) send_hex: bool,
    pub(crate) reply_hex: bool,
    pub(crate) ending: SerialLineEnding,
    pub(crate) crc: bool,
    pub(crate) page: SerialToolPage,
}

impl Default for SerialSenderPreferences {
    fn default() -> Self {
        Self {
            message: String::new(),
            pattern: String::new(),
            reply: String::new(),
            baud: String::new(),
            interval: "1000".into(),
            send_hex: false,
            reply_hex: false,
            ending: SerialLineEnding::default(),
            crc: false,
            page: SerialToolPage::default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SerialLogFilter {
    pub(crate) query: String,
    pub(crate) regex: bool,
    pub(crate) case_sensitive: bool,
}
