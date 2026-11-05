//! Telnet 自动登录. Telnet has no login of its own: the device asks in the
//! terminal (`login:`, `Password:`) and the answer is typed. This watches
//! what the device prints for those prompts and types the host's username
//! and password, each at most once. Pure, so the rules are tested here.

use zeroize::Zeroizing;

/// What devices print for a username: Linux's `login:`, Cisco's and
/// Huawei's `Username:`, and the odd `User Name:`.
const USER_PROMPTS: [&str; 3] = ["login:", "username:", "user name:"];
const PASSWORD_PROMPTS: [&str; 1] = ["password:"];

/// Only the end of the line matters: a longer one is cut from the front.
const LINE_LIMIT: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Nothing typed yet.
    User,
    /// The username went; the password may follow.
    Password,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prompt {
    User,
    Password,
}

/// Where the output is in an escape sequence, which is not text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Escape {
    None,
    Esc,
    Csi,
    /// OSC, DCS and the like, up to BEL or ST.
    String,
    StringEsc,
}

pub(crate) struct AutoLogin {
    encoding: crate::host::TerminalEncoding,
    user: String,
    password: Option<Zeroizing<String>>,
    user_prompts: Vec<String>,
    password_prompts: Vec<String>,
    /// The line the cursor is on, without escape sequences.
    line: Vec<u8>,
    escape: Escape,
    stage: Stage,
}

impl AutoLogin {
    /// An empty prompt means the usual ones. An empty user is for devices
    /// that ask only for a password; without a password, the user types it.
    pub fn new(
        user: &str,
        password: Option<Zeroizing<String>>,
        user_prompt: &str,
        password_prompt: &str,
    ) -> Self {
        let prompts = |custom: &str, usual: &[&str]| {
            let custom = custom.trim().to_lowercase();
            if custom.is_empty() {
                usual.iter().map(|prompt| prompt.to_string()).collect()
            } else {
                vec![custom]
            }
        };
        Self {
            encoding: crate::host::TerminalEncoding::Utf8,
            user: user.to_owned(),
            password: password.filter(|password| !password.is_empty()),
            user_prompts: prompts(user_prompt, &USER_PROMPTS),
            password_prompts: prompts(password_prompt, &PASSWORD_PROMPTS),
            line: Vec::new(),
            escape: Escape::None,
            stage: Stage::User,
        }
    }

    /// What the device printed.
    pub fn output(&mut self, data: &[u8]) {
        if self.stage == Stage::Done {
            return;
        }
        for &byte in data {
            match self.escape {
                Escape::None => match byte {
                    0x1b => self.escape = Escape::Esc,
                    b'\r' | b'\n' => self.line.clear(),
                    0x08 => {
                        self.line.pop();
                    }
                    0x00..=0x1f | 0x7f => {}
                    _ => self.line.push(byte),
                },
                Escape::Esc => {
                    self.escape = match byte {
                        b'[' => Escape::Csi,
                        b']' | b'P' | b'X' | b'^' | b'_' => Escape::String,
                        // `ESC ( B` and the like: more to come.
                        0x20..=0x2f => Escape::Esc,
                        _ => Escape::None,
                    }
                }
                Escape::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.escape = Escape::None;
                    }
                }
                Escape::String => match byte {
                    0x07 => self.escape = Escape::None,
                    0x1b => self.escape = Escape::StringEsc,
                    _ => {}
                },
                Escape::StringEsc => {
                    self.escape = if byte == b'\\' {
                        Escape::None
                    } else {
                        Escape::String
                    }
                }
            }
        }
        if self.line.len() > LINE_LIMIT {
            self.line.drain(..self.line.len() - LINE_LIMIT);
        }
    }

    pub fn with_encoding(mut self, encoding: crate::host::TerminalEncoding) -> Self {
        self.encoding = encoding;
        self
    }

    pub fn answer_encoded(&mut self) -> Result<Option<Zeroizing<Vec<u8>>>, gpui_kit::SharedString> {
        self.answer()
            .map(|bytes| crate::terminal::encode_secret(self.encoding, &bytes))
            .transpose()
    }

    /// Whether the line ends with a prompt still to be answered.
    pub fn waiting(&self) -> bool {
        self.prompt().is_some()
    }

    /// What to type for the prompt the line ends with: the username or the
    /// password, and Enter. Nothing when the user is to answer it: no
    /// password, no username, or a second username prompt after a failed
    /// login. Either way each prompt is answered once.
    pub fn answer(&mut self) -> Option<Zeroizing<Vec<u8>>> {
        let prompt = self.prompt()?;
        self.line.clear();
        match prompt {
            Prompt::Password => {
                self.stage = Stage::Done;
                self.password
                    .take()
                    .map(|password| line_of(password.as_bytes()))
            }
            Prompt::User if self.stage == Stage::User && !self.user.is_empty() => {
                self.stage = Stage::Password;
                Some(line_of(self.user.as_bytes()))
            }
            Prompt::User => {
                self.stage = Stage::Done;
                self.password = None;
                None
            }
        }
    }

    /// What the user typed: they log in themselves from here. Writes that
    /// start with ESC are the terminal answering the device (cursor reports,
    /// colors), or arrow keys, not a login.
    pub fn typed(&mut self, bytes: &[u8]) {
        if bytes.first().is_some_and(|byte| *byte != 0x1b) {
            self.stage = Stage::Done;
            self.password = None;
        }
    }

    pub fn is_done(&self) -> bool {
        self.stage == Stage::Done
    }

    fn prompt(&self) -> Option<Prompt> {
        if self.stage == Stage::Done {
            return None;
        }
        let bytes = crate::terminal::TerminalDecoder::new(self.encoding).decode(&self.line, false);
        let line = String::from_utf8_lossy(&bytes);
        let line = line.trim_end().to_lowercase();
        let ends =
            |prompts: &[String]| prompts.iter().any(|prompt| line.ends_with(prompt.as_str()));
        if ends(&self.password_prompts) {
            Some(Prompt::Password)
        } else if ends(&self.user_prompts) {
            Some(Prompt::User)
        } else {
            None
        }
    }
}

/// A line typed into the device: Enter is CR, which Telnet sends as CR LF.
fn line_of(text: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut line = Zeroizing::new(Vec::with_capacity(text.len() + 1));
    line.extend_from_slice(text);
    line.push(b'\r');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_prompts_and_credentials_follow_the_terminal_encoding() {
        for encoding in [
            crate::host::TerminalEncoding::Gbk,
            crate::host::TerminalEncoding::Gb18030,
        ] {
            let mut login = AutoLogin::new(
                "用户",
                Some(Zeroizing::new("密码".into())),
                "账号：",
                "口令：",
            )
            .with_encoding(encoding);
            for prompt in ["\x1b[31m账号：\x1b[0m", "\r\n口令："] {
                let (bytes, _, _) = encoding_rs::GBK.encode(prompt);
                for byte in bytes.as_ref() {
                    login.output(std::slice::from_ref(byte));
                }
                assert!(login.waiting());
                let answer = login.answer_encoded().unwrap().unwrap();
                if prompt.contains("账号") {
                    assert_eq!(&*answer, b"\xd3\xc3\xbb\xa7\r");
                } else {
                    assert_eq!(&*answer, b"\xc3\xdc\xc2\xeb\r");
                }
            }
        }
    }

    fn login(user: &str, password: Option<&str>) -> AutoLogin {
        AutoLogin::new(user, password.map(|p| Zeroizing::new(p.to_owned())), "", "")
    }

    fn answer(login: &mut AutoLogin) -> Option<String> {
        login
            .answer()
            .map(|line| String::from_utf8(line.to_vec()).unwrap())
    }

    #[test]
    fn linux_asks_for_login_then_password() {
        let mut auto = login("admin", Some("hunter2"));
        auto.output(b"Ubuntu 22.04 LTS\r\nweb-01 login: ");
        assert!(auto.waiting());
        assert_eq!(answer(&mut auto).as_deref(), Some("admin\r"));
        assert!(!auto.waiting(), "the echoed name is not a prompt");
        auto.output(b"admin\r\nPassword: ");
        assert_eq!(answer(&mut auto).as_deref(), Some("hunter2\r"));
        assert!(auto.is_done());
        auto.output(b"\r\nlogin: ");
        assert!(!auto.waiting());
    }

    #[test]
    fn network_devices_ask_with_any_case() {
        let mut auto = login("admin", Some("pw"));
        auto.output(b"\r\nUser Access Verification\r\n\r\nUsername:   ");
        assert_eq!(answer(&mut auto).as_deref(), Some("admin\r"));
        auto.output(b"\r\nPASSWORD:");
        assert_eq!(answer(&mut auto).as_deref(), Some("pw\r"));
    }

    #[test]
    fn a_prompt_may_come_in_pieces_and_colors() {
        let mut auto = login("admin", None);
        auto.output(b"\x1b]0;switch\x07\x1b(B\x1b[1mlog");
        assert!(!auto.waiting());
        auto.output(b"in:\x1b[0m ");
        assert!(auto.waiting());
        let mut auto = login("admin", None);
        auto.output(b"logx\x08in: ");
        assert!(auto.waiting(), "backspace takes the x back");
    }

    #[test]
    fn custom_prompts_replace_the_usual_ones() {
        let mut auto = AutoLogin::new(
            "admin",
            Some(Zeroizing::new("pw".into())),
            "账号：",
            "口令： ",
        );
        auto.output(b"login: ");
        assert!(!auto.waiting());
        auto.output("\r\n请输入账号：".as_bytes());
        assert_eq!(answer(&mut auto).as_deref(), Some("admin\r"));
        auto.output("\r\n口令：".as_bytes());
        assert_eq!(answer(&mut auto).as_deref(), Some("pw\r"));
    }

    #[test]
    fn a_device_asking_only_for_a_password_gets_only_the_password() {
        let mut auto = login("admin", Some("pw"));
        auto.output(b"\r\nPassword: ");
        assert_eq!(answer(&mut auto).as_deref(), Some("pw\r"));
        assert!(auto.is_done());

        let mut auto = login("", Some("pw"));
        auto.output(b"\r\nUsername: ");
        assert_eq!(answer(&mut auto), None, "no username to give");
        assert!(auto.is_done());
    }

    #[test]
    fn without_a_password_the_user_types_it() {
        let mut auto = login("admin", None);
        auto.output(b"login: ");
        assert_eq!(answer(&mut auto).as_deref(), Some("admin\r"));
        auto.output(b"admin\r\nPassword: ");
        assert!(auto.waiting());
        assert_eq!(answer(&mut auto), None);
        assert!(auto.is_done());
    }

    #[test]
    fn a_failed_login_is_not_tried_again() {
        let mut auto = login("admin", Some("pw"));
        auto.output(b"login: ");
        answer(&mut auto);
        auto.output(b"admin\r\n\r\nLogin incorrect\r\nlogin: ");
        assert_eq!(answer(&mut auto), None);
        assert!(auto.is_done());
    }

    #[test]
    fn typing_takes_over_but_terminal_replies_do_not() {
        let mut auto = login("admin", Some("pw"));
        auto.typed(b"\x1b[?1;2c");
        auto.typed(b"\x1b[A");
        assert!(!auto.is_done());
        auto.typed(b"r");
        assert!(auto.is_done());
        auto.output(b"login: ");
        assert!(!auto.waiting());
    }

    #[test]
    fn other_lines_are_not_prompts() {
        let mut auto = login("admin", Some("pw"));
        auto.output(b"Last login: Wed Oct 15 10:00:00 2026 from 10.0.0.1\r\n");
        auto.output(b"Your password expires in 3 days\r\n$ ");
        assert!(!auto.waiting());
        assert_eq!(answer(&mut auto), None);
        assert!(!auto.is_done());
    }
}
