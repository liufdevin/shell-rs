use std::{
    collections::HashMap,
    fmt,
    io::{Read as _, Write as _},
    process::{Child, Command, Stdio},
    sync::Mutex,
};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::i18n::t;

/// Internal startup argument for spawning an isolated WebView screen window.
pub const REMOTE_SCREEN_ARG: &str = "--remote-screen";

#[derive(Clone, Serialize, Deserialize)]
pub struct RemoteScreenSession {
    pub server_id: String,
    pub device_id: String,
    pub device_name: String,
    pub url: String,
    #[serde(default)]
    pub fallback_bearer: Option<String>,
}

impl fmt::Debug for RemoteScreenSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteScreenSession")
            .field("server_id", &self.server_id)
            .field("device_id", &self.device_id)
            .field("device_name", &self.device_name)
            .field("url", &redacted_url(&self.url))
            .field("has_fallback_bearer", &self.fallback_bearer.is_some())
            .finish()
    }
}

fn redacted_url(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

pub trait ScreenViewer: Send + Sync + 'static {
    fn open(&self, session: RemoteScreenSession) -> Result<()>;
    fn close_all(&self) {}
}

#[derive(Default)]
pub struct SystemScreenViewer {
    children: Mutex<HashMap<(String, String), Child>>,
}

impl SystemScreenViewer {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ScreenViewer for SystemScreenViewer {
    fn open(&self, session: RemoteScreenSession) -> Result<()> {
        let exe = std::env::current_exe().context("resolve current executable")?;
        let key = (session.server_id.clone(), session.device_id.clone());
        let payload = serde_json::to_vec(&session)?;

        let mut children = self
            .children
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        children.retain(|_, child| matches!(child.try_wait(), Ok(None)));
        if let Some(mut previous) = children.remove(&key) {
            let _ = previous.kill();
            let _ = previous.wait();
        }

        let mut command = Command::new(exe);
        command
            .arg(REMOTE_SCREEN_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().context("spawn remote screen window")?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&payload)?;
            stdin.write_all(b"\n")?;
            stdin.flush()?;
        }
        children.insert(key, child);
        Ok(())
    }

    fn close_all(&self) {
        let mut children = self
            .children
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (_, mut child) in children.drain() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for SystemScreenViewer {
    fn drop(&mut self) {
        self.close_all();
    }
}

/// Entry point for the isolated `--remote-screen` child process.
///
/// Reads a JSON [`RemoteScreenSession`] from stdin (so tokens never appear in
/// process command-line arguments) and opens a standalone native WebView window
/// backed by WebView2 (Windows), WKWebView (macOS), or WebKitGTK (Linux).
pub fn run_screen_viewer_from_stdin() -> Result<()> {
    let mut raw = Vec::new();
    std::io::stdin()
        .take(64 * 1024 + 1)
        .read_to_end(&mut raw)
        .context("read remote screen launch payload")?;
    ensure!(raw.len() <= 64 * 1024, "remote screen payload too large");
    let session: RemoteScreenSession =
        serde_json::from_slice(&raw).context("parse remote screen launch payload")?;
    validate_screen_url(&session.url)?;

    use tao::{
        dpi::LogicalSize,
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoop},
        window::WindowBuilder,
    };
    use wry::WebViewBuilder;

    let title = format!("{} — {}", session.device_name, t!("remote.screen"));
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_min_inner_size(LogicalSize::new(480.0, 360.0))
        .build(&event_loop)
        .context("create remote screen window")?;

    let mut builder = WebViewBuilder::new().with_url(&session.url);
    if let Some(bearer) = &session.fallback_bearer {
        builder =
            builder.with_initialization_script(fallback_auth_script(bearer, &session.device_id));
    }
    let _webview = builder
        .build(&window)
        .context("create remote screen webview")?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });
}

fn validate_screen_url(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).context("invalid remote screen url")?;
    let loopback = parsed.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|addr| addr.is_loopback())
    });
    ensure!(
        parsed.scheme() == "https" || (parsed.scheme() == "http" && loopback),
        "remote screen requires https or loopback http"
    );
    Ok(())
}

/// Fallback bootstrap script used only when connecting to an older
/// `baijie-remote` server that does not yet expose `POST /api/client/view-links`.
/// Injects the existing session bearer token on same-origin `/api/` fetches and
/// auto-selects the target device once loaded.
fn fallback_auth_script(bearer: &str, device_id: &str) -> String {
    let bearer_json = serde_json::to_string(bearer).unwrap_or_else(|_| "\"\"".into());
    let device_json = serde_json::to_string(device_id).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(function() {{
  const token = {bearer_json};
  const targetDeviceId = {device_json};
  const origFetch = window.fetch.bind(window);
  window.fetch = function(input, init) {{
    try {{
      const rawUrl = typeof input === 'string' ? input : (input && input.url ? input.url : '');
      const resolved = new URL(rawUrl, location.origin);
      if (resolved.origin === location.origin && resolved.pathname.startsWith('/api/')) {{
        const nextInit = Object.assign({{}}, init || {{}});
        const headers = new Headers(nextInit.headers || (typeof input !== 'string' && input && input.headers) || {{}});
        if (!headers.has('Authorization')) headers.set('Authorization', 'Bearer ' + token);
        if (!headers.has('X-Baijie-Request')) headers.set('X-Baijie-Request', '1');
        if (!headers.has('X-Baijie-Client')) headers.set('X-Baijie-Client', 'native');
        nextInit.headers = headers;
        return origFetch(input, nextInit);
      }}
    }} catch (_) {{}}
    return origFetch(input, init);
  }};
  let attempts = 0;
  const timer = setInterval(() => {{
    attempts++;
    if (attempts > 80) {{ clearInterval(timer); return; }}
    const item = document.querySelector(`[data-device-id="${{targetDeviceId}}"]`);
    if (item) {{
      item.click();
      const connectBtn = document.querySelector('#connect-button, #connect');
      if (connectBtn && !connectBtn.disabled) connectBtn.click();
      clearInterval(timer);
    }}
  }}, 150);
}})();"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_url_validation_allows_https_and_loopback_only() {
        assert!(
            validate_screen_url("https://remote.example.test:8180/#device=abc&access=123").is_ok()
        );
        assert!(validate_screen_url("http://127.0.0.1:8180/#device=abc&access=123").is_ok());
        assert!(validate_screen_url("http://remote.example.test:8180/").is_err());
        assert!(validate_screen_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn debug_format_redacts_access_hash_and_bearer() {
        let session = RemoteScreenSession {
            server_id: "srv-1".into(),
            device_id: "0123456789abcdef0123456789abcdef".into(),
            device_name: "A133-Board".into(),
            url: "https://example.test/#device=0123&access=secret-token".into(),
            fallback_bearer: Some("super-secret-bearer".into()),
        };
        let rendered = format!("{session:?}");
        assert!(!rendered.contains("secret-token"));
        assert!(!rendered.contains("super-secret-bearer"));
    }
}
