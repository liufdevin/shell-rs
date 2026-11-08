#[cfg(windows)]
mod windows;

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div,
};
use serde::Deserialize;

use super::{LanCommand, LanService, page::button};
use crate::i18n::t;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct FirewallReport {
    pub private_network: bool,
    pub firewall_enabled: bool,
    pub inbound_allowed: bool,
    pub local_rules_allowed: bool,
    pub tcp_rule: bool,
    pub udp_rule: bool,
    pub possible_blocks: u32,
}

impl FirewallReport {
    pub fn new(private_network: bool, tcp_rule: bool, udp_rule: bool) -> Self {
        Self {
            private_network,
            firewall_enabled: true,
            inbound_allowed: true,
            local_rules_allowed: true,
            tcp_rule,
            udp_rule,
            possible_blocks: 0,
        }
    }

    fn summary(&self) -> SharedString {
        if !self.firewall_enabled {
            t!("lan.firewall.disabled")
        } else if !self.inbound_allowed || !self.local_rules_allowed {
            t!("lan.firewall.managed")
        } else if self.possible_blocks > 0 {
            t!("lan.firewall.blocked", count = self.possible_blocks)
        } else if !self.tcp_rule || !self.udp_rule {
            t!("lan.firewall.missing")
        } else {
            t!("lan.firewall.ready")
        }
    }
}

#[derive(Clone, Debug)]
pub enum FirewallFailure {
    Cancelled,
    TimedOut,
    Unsupported,
    Failed(String),
}

impl FirewallFailure {
    fn describe(&self) -> SharedString {
        match self {
            Self::Cancelled => t!("lan.firewall.cancelled"),
            Self::TimedOut => t!("lan.firewall.timed_out"),
            Self::Unsupported => t!("lan.firewall.unsupported"),
            Self::Failed(reason) => t!("lan.firewall.failed", reason = reason),
        }
    }
}

pub trait FirewallBackend: Send + Sync + 'static {
    fn is_supported(&self) -> bool;
    fn check(&self) -> Result<FirewallReport, FirewallFailure>;
    fn configure(&self) -> Result<FirewallReport, FirewallFailure>;
}

pub(super) struct SystemFirewall;

impl FirewallBackend for SystemFirewall {
    fn is_supported(&self) -> bool {
        cfg!(windows)
    }
    fn check(&self) -> Result<FirewallReport, FirewallFailure> {
        #[cfg(windows)]
        {
            windows::check()
        }
        #[cfg(not(windows))]
        {
            Err(FirewallFailure::Unsupported)
        }
    }
    fn configure(&self) -> Result<FirewallReport, FirewallFailure> {
        #[cfg(windows)]
        {
            windows::configure()?;
            windows::check()
        }
        #[cfg(not(windows))]
        {
            Err(FirewallFailure::Unsupported)
        }
    }
}

pub(super) enum FirewallState {
    Unchecked,
    Checking,
    Configuring,
    Checked(FirewallReport),
    Failed(FirewallFailure),
}

impl FirewallState {
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Checking | Self::Configuring)
    }
    fn text(&self) -> SharedString {
        match self {
            Self::Unchecked => t!("lan.firewall.unchecked"),
            Self::Checking => t!("lan.firewall.checking"),
            Self::Configuring => t!("lan.firewall.configuring"),
            Self::Checked(report) => report.summary(),
            Self::Failed(error) => error.describe(),
        }
    }
}

impl LanService {
    pub fn set_firewall_backend(
        &mut self,
        backend: std::sync::Arc<dyn FirewallBackend>,
        cx: &mut Context<Self>,
    ) {
        if self.firewall_state.is_busy() {
            return;
        }
        self.firewall_backend = backend;
        self.firewall_state = FirewallState::Unchecked;
        cx.notify();
    }

    pub(super) fn firewall_operation(&mut self, configure: bool, cx: &mut Context<Self>) {
        if self.firewall_state.is_busy() {
            return;
        }
        if !self.firewall_backend.is_supported() {
            self.firewall_state = FirewallState::Failed(FirewallFailure::Unsupported);
            cx.notify();
            return;
        }
        self.firewall_state = if configure {
            FirewallState::Configuring
        } else {
            FirewallState::Checking
        };
        let backend = self.firewall_backend.clone();
        self.firewall_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    if configure {
                        backend.configure()
                    } else {
                        backend.check()
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.firewall_task = None;
                this.firewall_state = match result {
                    Ok(report) => FirewallState::Checked(report),
                    Err(error) => FirewallState::Failed(error),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }
}

pub(super) fn section(service: &LanService, cx: &App) -> Option<gpui_kit::AnyElement> {
    if !service.firewall_backend.is_supported() {
        return None;
    }
    let busy = service.firewall_state.is_busy();
    let status = service.firewall_state.text();
    let mut view = v_flex()
        .gap_2()
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(button(
                    "lan-firewall-check",
                    t!("lan.firewall.check"),
                    LanCommand::CheckFirewall,
                    busy,
                ))
                .child(button(
                    "lan-firewall-configure",
                    t!("lan.firewall.configure"),
                    LanCommand::ConfigureFirewall,
                    busy,
                )),
        )
        .child(
            div()
                .id("lan-firewall-status")
                .role(gpui_kit::Role::Status)
                .test_support()
                .aria_label(status.clone())
                .text_sm()
                .child(status),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(t!("lan.firewall.scope")),
        );
    if let FirewallState::Checked(report) = &service.firewall_state
        && !report.private_network
    {
        view = view.child(
            div()
                .id("lan-firewall-network-warning")
                .test_support()
                .text_sm()
                .child(t!("lan.firewall.no_private_network")),
        );
    }
    Some(view.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_or_unexpected_reports_never_become_ready() {
        for json in [
            "{}",
            r#"{"tcp_rule":true,"udp_rule":true}"#,
            r#"{"private_network":true,"firewall_enabled":true,"inbound_allowed":true,"local_rules_allowed":true,"tcp_rule":true,"udp_rule":true,"possible_blocks":0,"unexpected":true}"#,
        ] {
            assert!(serde_json::from_str::<FirewallReport>(json).is_err());
        }
    }

    #[test]
    fn report_warnings_and_cancellation_do_not_claim_success() {
        crate::i18n::isolate_thread();
        let ready = FirewallReport::new(true, true, true).summary();
        let mut report = FirewallReport::new(true, false, true);
        assert_ne!(report.summary(), ready);
        report.tcp_rule = true;
        report.firewall_enabled = false;
        assert_ne!(report.summary(), ready);
        report.firewall_enabled = true;
        report.local_rules_allowed = false;
        assert_ne!(report.summary(), ready);
        report.local_rules_allowed = true;
        report.possible_blocks = 1;
        assert_ne!(report.summary(), ready);
        assert_ne!(FirewallFailure::Cancelled.describe(), ready);
        assert!(FirewallState::Checking.is_busy());
        assert!(FirewallState::Configuring.is_busy());
        assert!(!FirewallState::Checked(report).is_busy());
    }
}
