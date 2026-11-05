#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalEncoding {
    #[default]
    Utf8,
    Gbk,
    Gb18030,
}

impl TerminalEncoding {
    pub const ALL: [Self; 3] = [Self::Utf8, Self::Gbk, Self::Gb18030];

    pub fn key(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Gbk => "gbk",
            Self::Gb18030 => "gb18030",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Gbk => "GBK",
            Self::Gb18030 => "GB18030",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|encoding| encoding.key() == key)
    }
}
