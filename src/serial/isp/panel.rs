use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use super::{Firmware, IspEvent, IspRequest, stc};
use crate::{
    i18n::t,
    settings::SettingsStore,
    terminal::{TerminalLifecycle, TerminalView},
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _,
    button::Button,
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    progress::Progress,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IspPage {
    #[default]
    MultiSend,
    Stm32,
    Stc15,
}

impl IspPage {
    pub(crate) const ALL: [Self; 3] = [Self::MultiSend, Self::Stm32, Self::Stc15];
    pub(crate) fn label(self) -> SharedString {
        match self {
            Self::MultiSend => t!("serial.assistant.multi_send"),
            Self::Stm32 => "STM32/GD32 ISP".into(),
            Self::Stc15 => "STC/IAP15 ISP".into(),
        }
    }
    pub(crate) fn compact_label(self) -> SharedString {
        match self {
            Self::MultiSend => t!("serial.assistant.multi_send"),
            Self::Stm32 => "STM32/GD32".into(),
            Self::Stc15 => "STC/IAP15".into(),
        }
    }
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::MultiSend => "serial-page-multi",
            Self::Stm32 => "serial-page-stm32",
            Self::Stc15 => "serial-page-stc15",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IspCommand {
    Page(IspPage),
    ChooseFirmware,
    ChoosePython,
    Detect,
    Start,
    ConfirmStart,
    Stop,
    Verify(bool),
    Run(bool),
    Protocol(String),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct IspPreferences {
    stm_firmware: String,
    stc_firmware: String,
    address: String,
    baud: String,
    python: String,
    protocol: String,
    verify: bool,
    run: bool,
}
impl Default for IspPreferences {
    fn default() -> Self {
        Self {
            stm_firmware: String::new(),
            stc_firmware: String::new(),
            address: "0x08000000".into(),
            baud: "115200".into(),
            python: "python".into(),
            protocol: "auto".into(),
            verify: true,
            run: true,
        }
    }
}

pub(crate) struct IspPanel {
    page: IspPage,
    dispatch: FocusHandle,
    target: Option<WeakEntity<TerminalView>>,
    settings: Entity<SettingsStore>,
    stm_firmware: Entity<InputState>,
    stc_firmware: Entity<InputState>,
    address: Entity<InputState>,
    baud: Entity<InputState>,
    python: Entity<InputState>,
    protocol: String,
    verify: bool,
    run: bool,
    task: Option<Task<()>>,
    picker: Option<Task<()>>,
    cancel: Arc<AtomicBool>,
    status: SharedString,
    identity: SharedString,
    log: std::collections::VecDeque<SharedString>,
    error: bool,
    percent: u8,
    _subscriptions: Vec<Subscription>,
}

impl IspPanel {
    pub(crate) fn new(
        dispatch: FocusHandle,
        settings: Entity<SettingsStore>,
        page: IspPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let saved = settings.read(cx).serial_assistant().isp.clone();
        let mut this = Self {
            page,
            dispatch,
            target: None,
            settings,
            stm_firmware: cx
                .new(|cx| InputState::new(window, cx).default_value(saved.stm_firmware)),
            stc_firmware: cx
                .new(|cx| InputState::new(window, cx).default_value(saved.stc_firmware)),
            address: cx.new(|cx| InputState::new(window, cx).default_value(saved.address)),
            baud: cx.new(|cx| InputState::new(window, cx).default_value(saved.baud)),
            python: cx.new(|cx| InputState::new(window, cx).default_value(saved.python)),
            protocol: saved.protocol,
            verify: saved.verify,
            run: saved.run,
            task: None,
            picker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            status: t!("serial.isp.ready"),
            identity: SharedString::default(),
            log: std::collections::VecDeque::new(),
            error: false,
            percent: 0,
            _subscriptions: Vec::new(),
        };
        for input in [
            &this.stm_firmware,
            &this.stc_firmware,
            &this.address,
            &this.baud,
            &this.python,
        ] {
            this._subscriptions
                .push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.save(cx);
                    }
                }));
        }
        this
    }

    fn save(&self, cx: &mut Context<Self>) {
        let saved = IspPreferences {
            stm_firmware: self.stm_firmware.read(cx).value().to_string(),
            stc_firmware: self.stc_firmware.read(cx).value().to_string(),
            address: self.address.read(cx).value().to_string(),
            baud: self.baud.read(cx).value().to_string(),
            python: self.python.read(cx).value().to_string(),
            protocol: self.protocol.clone(),
            verify: self.verify,
            run: self.run,
        };
        self.settings.update(cx, |settings, cx| {
            settings.update_serial_assistant(|preferences| preferences.isp = saved, cx)
        });
    }

    pub(crate) fn busy(&self) -> bool {
        self.task.is_some() || self.picker.is_some()
    }

    pub(crate) fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let defaults = IspPreferences::default();
        for (input, value) in [
            (&self.stm_firmware, defaults.stm_firmware),
            (&self.stc_firmware, defaults.stc_firmware),
            (&self.address, defaults.address),
            (&self.baud, defaults.baud),
            (&self.python, defaults.python),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.protocol = defaults.protocol;
        self.verify = defaults.verify;
        self.run = defaults.run;
        self.page = IspPage::MultiSend;
        self.status = t!("serial.isp.ready");
        self.identity = SharedString::default();
        self.log.clear();
        self.percent = 0;
        self.error = false;
        self.save(cx);
        cx.notify();
    }

    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.task.is_some() {
            self.cancel.store(true, Ordering::Release);
            self.status = t!("serial.isp.stopping");
            cx.notify();
        }
    }

    pub(crate) fn set_target(
        &mut self,
        target: Option<WeakEntity<TerminalView>>,
        cx: &mut Context<Self>,
    ) {
        if self.target != target {
            self.cancel(cx);
            self.target = target;
            cx.notify();
        }
    }

    pub(crate) fn command(
        &mut self,
        command: &IspCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(command, IspCommand::Stop) {
            self.cancel(cx);
            return;
        }
        if self.busy() {
            return;
        }
        match command {
            IspCommand::Page(page) => {
                self.page = *page;
                self.status = t!("serial.isp.ready");
                self.error = false;
                self.percent = 0;
                self.identity = SharedString::default();
                self.log.clear();
            }
            IspCommand::ChooseFirmware => self.pick(false, window, cx),
            IspCommand::ChoosePython => self.pick(true, window, cx),
            IspCommand::Detect | IspCommand::ConfirmStart => {
                if let Err(error) = self.start(matches!(command, IspCommand::Detect), cx) {
                    self.status = error;
                    self.error = true;
                }
            }
            IspCommand::Verify(value) => self.verify = *value,
            IspCommand::Run(value) => self.run = *value,
            IspCommand::Protocol(value)
                if ["auto", "stc15", "stc15a"].contains(&value.as_str()) =>
            {
                self.protocol = value.clone()
            }
            _ => {}
        }
        self.save(cx);
        cx.notify();
    }

    fn pick(&mut self, python: bool, window: &mut Window, cx: &mut Context<Self>) {
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(if python {
                t!("serial.isp.choose_python")
            } else {
                t!("serial.isp.choose_firmware")
            }),
        });
        let input = if python {
            self.python.clone()
        } else {
            self.firmware_input().clone()
        };
        self.picker = Some(cx.spawn_in(window, async move |owner, cx| {
            let result = answer.await;
            let _ = owner.update_in(cx, |this, window, cx| {
                this.picker = None;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.first() {
                            input.update(cx, |input, cx| {
                                input.set_value(path.to_string_lossy().to_string(), window, cx)
                            });
                        }
                    }
                    Ok(Err(error)) => {
                        this.status = error.to_string().into();
                        this.error = true;
                    }
                    _ => {}
                }
                cx.notify();
            });
        }));
    }

    fn firmware_input(&self) -> &Entity<InputState> {
        if self.page == IspPage::Stc15 {
            &self.stc_firmware
        } else {
            &self.stm_firmware
        }
    }

    fn start(&mut self, detect: bool, cx: &mut Context<Self>) -> Result<(), SharedString> {
        if self.page == IspPage::MultiSend {
            return Err(t!("serial.tool.unavailable"));
        }
        let target = self
            .target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .filter(|view| {
                view.read(cx).lifecycle(cx) == TerminalLifecycle::Running
                    && !view.read(cx).serial_isp_active(cx)
            })
            .ok_or_else(|| t!("serial.tool.unavailable"))?;
        let baud = self
            .baud
            .read(cx)
            .value()
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|baud| (1200..=1_000_000).contains(baud))
            .ok_or_else(|| t!("serial.isp.invalid_settings"))?;
        let at = self.address.read(cx).value();
        let address = if self.page == IspPage::Stc15 {
            0
        } else {
            u32::from_str_radix(
                at.trim().trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .map_err(|_| t!("serial.isp.invalid_settings"))?
        };
        let path = PathBuf::from(self.firmware_input().read(cx).value().trim());
        if !detect && path.as_os_str().is_empty() {
            return Err(t!("serial.isp.select_firmware"));
        }
        let stc = (self.page == IspPage::Stc15).then(|| stc::Options {
            python: self.python.read(cx).value().trim().to_string(),
            protocol: self.protocol.clone(),
        });
        if stc
            .as_ref()
            .is_some_and(|options| options.python.is_empty())
        {
            return Err(t!("serial.isp.invalid_settings"));
        }
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (verify, run) = (self.verify, self.run);
        let generation = target.read(cx).generation(cx);
        let target = target.downgrade();
        self.percent = 0;
        self.error = false;
        self.identity = SharedString::default();
        self.log.clear();
        self.status = t!("serial.isp.preparing");
        self.task = Some(cx.spawn(async move |owner, cx| {
            let image = if detect {
                Ok(None)
            } else {
                cx.background_spawn(async move { Firmware::load(&path, address).map(Some) })
                    .await
            };
            let result = async {
                let image = image?;
                if cancel.load(Ordering::Acquire) {
                    return Err(t!("serial.isp.cancelled").to_string());
                }
                if stc.is_some()
                    && image
                        .as_ref()
                        .is_some_and(|image| image.address != 0 || image.bytes.len() > 65536)
                {
                    return Err(t!("serial.isp.stc_range").to_string());
                }
                let (send, receive) = mpsc::sync_channel(128);
                owner
                    .update(cx, |_, cx| {
                        let view = target
                            .upgrade()
                            .filter(|view| view.read(cx).generation(cx) == generation)
                            .ok_or_else(|| t!("serial.tool.unavailable").to_string())?;
                        view.update(cx, |view, cx| {
                            view.start_serial_isp(
                                IspRequest {
                                    image,
                                    baud,
                                    verify,
                                    run,
                                    stc,
                                    cancelled: cancel.clone(),
                                    active: Arc::new(AtomicBool::new(false)),
                                    events: send,
                                },
                                cx,
                            )
                        })
                        .map_err(|error| error.to_string())
                    })
                    .map_err(|_| t!("serial.tool.unavailable").to_string())??;
                loop {
                    match receive.try_recv() {
                        Ok(IspEvent::Identity(identity)) => {
                            let _ = owner.update(cx, |this, cx| {
                                this.identity = identity.into();
                                cx.notify();
                            });
                        }
                        Ok(IspEvent::Finished(result)) => return result,
                        Ok(IspEvent::Progress { percent, message }) => {
                            let _ = owner.update(cx, |this, cx| {
                                this.percent = percent;
                                if this.log.len() >= 64 {
                                    this.log.pop_front();
                                }
                                this.log.push_back(
                                    message.chars().take(2048).collect::<String>().into(),
                                );
                                if !cancel.load(Ordering::Acquire) {
                                    this.status =
                                        message.chars().take(2048).collect::<String>().into();
                                }
                                cx.notify();
                            });
                        }
                        Err(mpsc::TryRecvError::Disconnected) => {
                            return Err(t!("serial.isp.transport_closed").to_string());
                        }
                        Err(mpsc::TryRecvError::Empty) => {
                            cx.background_executor()
                                .timer(Duration::from_millis(20))
                                .await
                        }
                    }
                }
            }
            .await;
            let _ = owner.update(cx, |this, cx| {
                this.task = None;
                this.error = result.is_err();
                this.status = match result {
                    Ok(()) => {
                        this.percent = 100;
                        if detect {
                            t!("serial.isp.detected")
                        } else {
                            t!("serial.isp.complete")
                        }
                    }
                    Err(error) => {
                        if this.log.len() >= 64 {
                            this.log.pop_front();
                        }
                        this.log
                            .push_back(error.chars().take(2048).collect::<String>().into());
                        error.chars().take(240).collect::<String>().into()
                    }
                };
                if let Some(view) = target.upgrade() {
                    view.update(cx, |_, cx| cx.notify());
                }
                cx.notify();
            });
        }));
        Ok(())
    }

    fn button(&self, id: &'static str, label: SharedString, command: IspCommand) -> Button {
        let dispatch = self.dispatch.clone();
        Button::new(id)
            .small()
            .label(label)
            .on_click(move |_, window, cx| {
                dispatch.dispatch_action(
                    &crate::app::SerialAssistantAction(
                        crate::serial::SerialAssistantTabCommand::Isp(command.clone()),
                    ),
                    window,
                    cx,
                );
            })
    }
}

impl Drop for IspPanel {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl Render for IspPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy();
        let stc = self.page == IspPage::Stc15;
        let running = self
            .target
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_some_and(|view| view.read(cx).lifecycle(cx) == TerminalLifecycle::Running);
        let field = |label: SharedString, input: &Entity<InputState>, id| {
            v_flex()
                .gap_1()
                .child(div().text_xs().child(label))
                .child(Input::new(input).id(id).small().disabled(busy))
        };
        v_flex()
            .id("serial-isp")
            .test_support()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(
                v_flex()
                    .p_2()
                    .gap_2()
                    .flex_shrink_0()
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                self.button(
                                    "serial-isp-detect",
                                    t!("serial.isp.detect"),
                                    IspCommand::Detect,
                                )
                                .disabled(busy || !running),
                            )
                            .child(
                                self.button(
                                    "serial-isp-start",
                                    t!("serial.isp.flash_dialog"),
                                    IspCommand::Start,
                                )
                                .disabled(busy || !running),
                            )
                            .child(
                                self.button(
                                    "serial-isp-stop",
                                    t!("serial.assistant.stop"),
                                    IspCommand::Stop,
                                )
                                .outline()
                                .disabled(self.task.is_none()),
                            ),
                    )
                    .child(Progress::new("serial-isp-progress").value(f32::from(self.percent)))
                    .child(
                        div()
                            .id("serial-isp-chip")
                            .test_support()
                            .aria_label(self.identity.clone())
                            .text_xs()
                            .child(self.identity.clone()),
                    )
                    .child(
                        div()
                            .id("serial-isp-status")
                            .test_support()
                            .role(Role::Status)
                            .aria_label(self.status.clone())
                            .max_h(rems(5.))
                            .overflow_y_scroll()
                            .text_xs()
                            .text_color(if self.error {
                                cx.theme().danger
                            } else {
                                cx.theme().foreground
                            })
                            .child(self.status.clone()),
                    ),
            )
            .child(
                v_flex()
                    .id("serial-isp-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_3()
                    .p_2()
                    .child(field(
                        t!("serial.isp.firmware"),
                        self.firmware_input(),
                        "serial-isp-firmware",
                    ))
                    .child(
                        self.button(
                            "serial-isp-pick-firmware",
                            t!("serial.isp.choose_firmware"),
                            IspCommand::ChooseFirmware,
                        )
                        .outline()
                        .disabled(busy),
                    )
                    .child(field(t!("serial.isp.baud"), &self.baud, "serial-isp-baud"))
                    .when(!stc, |body| {
                        let dispatch = self.dispatch.clone();
                        let go = dispatch.clone();
                        body.child(field(
                            t!("serial.isp.address"),
                            &self.address,
                            "serial-isp-address",
                        ))
                        .child(
                            Checkbox::new("serial-isp-verify")
                                .small()
                                .w_auto()
                                .label(t!("serial.isp.verify"))
                                .checked(self.verify)
                                .disabled(busy)
                                .on_click(move |value, window, cx| {
                                    dispatch.dispatch_action(
                                        &crate::app::SerialAssistantAction(
                                            crate::serial::SerialAssistantTabCommand::Isp(
                                                IspCommand::Verify(*value),
                                            ),
                                        ),
                                        window,
                                        cx,
                                    )
                                }),
                        )
                        .child(
                            Checkbox::new("serial-isp-run")
                                .small()
                                .w_auto()
                                .label(t!("serial.isp.run"))
                                .checked(self.run)
                                .disabled(busy)
                                .on_click(move |value, window, cx| {
                                    go.dispatch_action(
                                        &crate::app::SerialAssistantAction(
                                            crate::serial::SerialAssistantTabCommand::Isp(
                                                IspCommand::Run(*value),
                                            ),
                                        ),
                                        window,
                                        cx,
                                    )
                                }),
                        )
                    })
                    .when(stc, |body| {
                        body.child(field(
                            t!("serial.isp.python"),
                            &self.python,
                            "serial-isp-python",
                        ))
                        .child(
                            self.button(
                                "serial-isp-pick-python",
                                t!("serial.isp.choose_python"),
                                IspCommand::ChoosePython,
                            )
                            .outline()
                            .disabled(busy),
                        )
                        .child(h_flex().gap_1().flex_wrap().children(
                            ["auto", "stc15", "stc15a"].map(|protocol| {
                                self.button(
                                    match protocol {
                                        "auto" => "serial-isp-auto",
                                        "stc15" => "serial-isp-stc15",
                                        _ => "serial-isp-stc15a",
                                    },
                                    if protocol == "auto" {
                                        t!("serial.isp.auto")
                                    } else {
                                        protocol.into()
                                    },
                                    IspCommand::Protocol(protocol.into()),
                                )
                                .selected(self.protocol == protocol)
                                .disabled(busy)
                            }),
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("serial.isp.stc_help")),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if stc {
                                t!("serial.isp.stc_waiting")
                            } else {
                                t!("serial.isp.stm_help")
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .child(t!("serial.isp.erase_hint")),
                    )
                    .child(
                        v_flex()
                            .id("serial-isp-log")
                            .test_support()
                            .gap_1()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .children(self.log.iter().cloned().map(|line| div().child(line))),
                    ),
            )
    }
}
