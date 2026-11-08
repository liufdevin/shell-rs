use super::{
    AdbConnection, AdbDetection, AdbDetector, AndroidApp, AndroidCapabilities, AndroidTools,
};
use crate::{app::AdbDeviceAction, i18n::t};
use anyhow::Result;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    component::{
        ActiveTheme as _, Disableable as _, Sizable as _,
        button::Button,
        checkbox::Checkbox,
        h_flex,
        input::{Input, InputEvent, InputState},
        searchable_list::SearchableListItem,
        select::{Select, SelectEvent, SelectState},
        v_flex,
    },
    *,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceCommand {
    Refresh,
    Select(String),
    StartMirror,
    StopMirror,
    SetMirrorAudio(bool),
    InstallApk,
    InstallApkPath(PathBuf),
    RefreshApps,
    Launch,
    Cancel,
    ChooseScrcpy,
}

#[derive(Clone)]
struct DeviceChoice {
    id: String,
    label: SharedString,
    device: AdbConnection,
    capabilities: AndroidCapabilities,
    problem: Option<String>,
}
impl SearchableListItem for DeviceChoice {
    type Value = String;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &String {
        &self.id
    }
}
#[derive(Clone)]
struct AppChoice(AndroidApp);
impl SearchableListItem for AppChoice {
    type Value = String;
    fn title(&self) -> SharedString {
        self.0.package().to_owned().into()
    }
    fn value(&self) -> &String {
        self.0.package_ref()
    }
}

struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
struct MirrorState {
    id: u64,
    _cancel: Cancellation,
    running: bool,
    audio: bool,
}
enum Message {
    Scanned(u64, Result<(Vec<DeviceChoice>, Option<PathBuf>), String>),
    Apps(u64, Result<Vec<AndroidApp>, String>),
    Finished(u64, bool, Result<(), String>),
    MirrorStarted(String, u64),
    MirrorEnded(String, u64, Result<(), String>),
}

pub struct DeviceManager {
    detector: Arc<dyn AdbDetector>,
    tools: Arc<dyn AndroidTools>,
    dispatch: FocusHandle,
    picker: Entity<SelectState<Vec<DeviceChoice>>>,
    apps: Entity<SelectState<Vec<AppChoice>>>,
    package: Entity<InputState>,
    devices: Vec<DeviceChoice>,
    selected: Option<String>,
    revision: u64,
    mirror_id: u64,
    scan: Option<Cancellation>,
    job: Option<Cancellation>,
    mirrors: HashMap<String, MirrorState>,
    mirror_workers: Vec<std::thread::JoinHandle<()>>,
    scrcpy: Option<PathBuf>,
    mirror_audio: bool,
    status: Option<SharedString>,
    error: Option<SharedString>,
    sender: mpsc::Sender<Message>,
    _poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl DeviceManager {
    pub fn new(
        detector: Arc<dyn AdbDetector>,
        tools: Arc<dyn AndroidTools>,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let picker = cx.new(|cx| {
            SelectState::new(Vec::<DeviceChoice>::new(), None, window, cx).searchable(true)
        });
        let apps = cx
            .new(|cx| SelectState::new(Vec::<AppChoice>::new(), None, window, cx).searchable(true));
        let package = cx.new(|cx| InputState::new(window, cx).placeholder("com.example.app"));
        let subscriptions = vec![
            cx.subscribe_in(&picker, window, {
                let dispatch = dispatch.clone();
                move |_, _, event, window, cx| {
                    if let SelectEvent::Confirm(Some(serial)) = event {
                        let dispatch = dispatch.clone();
                        let command = DeviceCommand::Select(serial.clone());
                        window.defer(cx, move |window, cx| {
                            dispatch.dispatch_action(&AdbDeviceAction(command), window, cx);
                        });
                    }
                }
            }),
            cx.subscribe_in(&apps, window, |this, _, event, window, cx| {
                if let SelectEvent::Confirm(Some(package)) = event {
                    this.package
                        .update(cx, |input, cx| input.set_value(package, window, cx));
                }
            }),
            cx.subscribe_in(&package, window, {
                let dispatch = dispatch.clone();
                move |_, _, event, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        let dispatch = dispatch.clone();
                        window.defer(cx, move |window, cx| {
                            dispatch.dispatch_action(
                                &AdbDeviceAction(DeviceCommand::Launch),
                                window,
                                cx,
                            );
                        });
                    }
                }
            }),
            cx.on_app_quit(|this, cx| {
                let workers = this.shutdown();
                cx.background_spawn(async move {
                    for worker in workers {
                        let _ = worker.join();
                    }
                })
            }),
            cx.on_release(|this, _| {
                this.shutdown();
            }),
        ];
        let (sender, receiver) = mpsc::channel();
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let messages: Vec<_> = receiver.try_iter().take(64).collect();
                if !messages.is_empty()
                    && this
                        .update_in(cx, |this, window, cx| {
                            for message in messages {
                                this.receive(message, window, cx);
                            }
                        })
                        .is_err()
                {
                    break;
                }
            }
        });
        Self {
            detector,
            tools,
            dispatch,
            picker,
            apps,
            package,
            devices: Vec::new(),
            selected: None,
            revision: 0,
            mirror_id: 0,
            scan: None,
            job: None,
            mirrors: HashMap::new(),
            mirror_workers: Vec::new(),
            scrcpy: None,
            mirror_audio: false,
            status: None,
            error: None,
            sender,
            _poll: poll,
            _subscriptions: subscriptions,
        }
    }

    fn selected(&self) -> Option<&DeviceChoice> {
        self.selected
            .as_ref()
            .and_then(|serial| self.devices.iter().find(|device| &device.id == serial))
    }

    pub fn command(&mut self, command: DeviceCommand, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            DeviceCommand::Refresh => self.refresh(cx),
            DeviceCommand::Select(serial) => {
                if self.scan.is_some()
                    || self.selected.as_ref() == Some(&serial)
                    || !self.devices.iter().any(|device| device.id == serial)
                {
                    return;
                }
                self.revision += 1;
                self.job = None;
                self.selected = Some(serial);
                self.error = None;
                self.status = None;
                self.clear_apps(window, cx);
                cx.notify();
            }
            DeviceCommand::Cancel => {
                self.revision += 1;
                self.job = None;
                self.scan = None;
                self.status = Some(t!("android.cancelled"));
                cx.notify();
            }
            DeviceCommand::StartMirror => self.start_mirror(cx),
            DeviceCommand::StopMirror => {
                if let Some(id) = &self.selected {
                    self.mirrors.remove(id);
                }
                cx.notify();
            }
            DeviceCommand::SetMirrorAudio(audio) => {
                if self.scan.is_none()
                    && self.selected().is_some_and(|device| {
                        device.capabilities.is_mirrorable()
                            && device.capabilities.sdk() >= 30
                            && !self.mirrors.contains_key(&device.id)
                    })
                {
                    self.mirror_audio = audio;
                    cx.notify();
                }
            }
            DeviceCommand::RefreshApps => self.load_apps(cx),
            DeviceCommand::Launch => {
                if self.job.is_some()
                    || self.scan.is_some()
                    || self
                        .selected()
                        .is_none_or(|device| !device.capabilities.is_launchable())
                {
                    return;
                }
                let package = self.package.read(cx).value().trim().to_owned();
                self.start_job(
                    false,
                    move |tools, device, cancel| tools.launch(device, &package, cancel),
                    cx,
                );
            }
            DeviceCommand::InstallApk => self.choose_file(false, window, cx),
            DeviceCommand::InstallApkPath(path) => self.install(path, cx),
            DeviceCommand::ChooseScrcpy => self.choose_file(true, window, cx),
        }
    }

    fn clear_apps(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.apps.update(cx, |apps, cx| {
            apps.set_selected_index(None, window, cx);
            apps.set_items(Vec::new(), window, cx);
            cx.notify();
        });
        self.package
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        self.job = None;
        self.scan = None;
        self.error = None;
        self.status = None;
        let token = Cancellation::new();
        let cancel = token.0.clone();
        self.scan = Some(token);
        let revision = self.revision;
        let detector = self.detector.clone();
        let tools = self.tools.clone();
        let sender = self.sender.clone();
        let spawned = std::thread::Builder::new()
            .name("shelllf-android-discover".into())
            .spawn(move || {
                let result = match detector.detect(None) {
                    AdbDetection::Matched(device) => Ok(vec![device]),
                    AdbDetection::Choose(devices) => Ok(devices),
                    AdbDetection::Unavailable(error) => Err(if error.is_empty() {
                        t!("adb.unavailable").to_string()
                    } else {
                        error
                    }),
                }
                .map(|devices| {
                    let mut choices = Vec::new();
                    for device in devices {
                        if cancel.load(Ordering::Acquire) {
                            break;
                        }
                        let (capabilities, problem) = match tools.probe(&device, &cancel) {
                            Ok(capabilities) => (capabilities, None),
                            Err(error) => (AndroidCapabilities::default(), Some(error.to_string())),
                        };
                        let label = device_label(&device);
                        choices.push(DeviceChoice {
                            id: device.serial().to_owned(),
                            label,
                            device,
                            capabilities,
                            problem,
                        });
                    }
                    (choices, tools.scrcpy())
                });
                if !cancel.load(Ordering::Acquire) {
                    let _ = sender.send(Message::Scanned(revision, result));
                }
            });
        if let Err(error) = spawned {
            self.scan = None;
            self.error = Some(error.to_string().into());
        }
        cx.notify();
    }

    fn install(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.job.is_some()
            || self.scan.is_some()
            || self
                .selected()
                .is_none_or(|device| !device.capabilities.is_installable())
        {
            return;
        }
        self.start_job(
            true,
            move |tools, device, cancel| tools.install(device, &path, cancel),
            cx,
        );
    }
    fn start_job(
        &mut self,
        install: bool,
        run: impl FnOnce(&dyn AndroidTools, &AdbConnection, &AtomicBool) -> Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(device) = self.selected().map(|device| device.device.clone()) else {
            return;
        };
        self.error = None;
        self.status = None;
        let token = Cancellation::new();
        let cancel = token.0.clone();
        self.job = Some(token);
        let tools = self.tools.clone();
        let sender = self.sender.clone();
        let revision = self.revision;
        if let Err(error) = std::thread::Builder::new()
            .name("shelllf-android-command".into())
            .spawn(move || {
                let result =
                    run(tools.as_ref(), &device, &cancel).map_err(|error| error.to_string());
                if !cancel.load(Ordering::Acquire) {
                    let _ = sender.send(Message::Finished(revision, install, result));
                }
            })
        {
            self.job = None;
            self.error = Some(error.to_string().into());
        }
        cx.notify();
    }
    fn load_apps(&mut self, cx: &mut Context<Self>) {
        if self.scan.is_some()
            || self.job.is_some()
            || self
                .selected()
                .is_none_or(|device| !device.capabilities.is_launchable())
        {
            return;
        }
        let device = self.selected().unwrap().device.clone();
        let tools = self.tools.clone();
        let sender = self.sender.clone();
        let revision = self.revision;
        let token = Cancellation::new();
        let cancel = token.0.clone();
        self.job = Some(token);
        self.error = None;
        self.status = None;
        if let Err(error) = std::thread::Builder::new()
            .name("shelllf-android-apps".into())
            .spawn(move || {
                let result = tools
                    .apps(&device, &cancel)
                    .map_err(|error| error.to_string());
                if !cancel.load(Ordering::Acquire) {
                    let _ = sender.send(Message::Apps(revision, result));
                }
            })
        {
            self.job = None;
            self.error = Some(error.to_string().into());
        }
        cx.notify();
    }
    fn choose_file(&mut self, scrcpy: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() || self.scan.is_some() {
            return;
        }
        let Some(device) = self.selected() else {
            return;
        };
        if (scrcpy && !device.capabilities.is_mirrorable())
            || (!scrcpy && !device.capabilities.is_installable())
        {
            return;
        }
        let revision = self.revision;
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(if scrcpy {
                t!("android.choose_scrcpy")
            } else {
                t!("android.install")
            }),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                if this.revision != revision || this.job.is_some() || this.scan.is_some() {
                    return;
                }
                if scrcpy {
                    match super::mirror::validate_scrcpy(&path) {
                        Ok(path) => {
                            this.scrcpy = Some(path);
                            this.error = None;
                        }
                        Err(error) => this.error = Some(error.to_string().into()),
                    }
                    cx.notify();
                } else {
                    let dispatch = this.dispatch.clone();
                    let manager = cx.entity().downgrade();
                    window.defer(cx, move |window, cx| {
                        if manager
                            .upgrade()
                            .is_some_and(|manager| manager.read(cx).revision == revision)
                        {
                            dispatch.dispatch_action(
                                &AdbDeviceAction(DeviceCommand::InstallApkPath(path)),
                                window,
                                cx,
                            );
                        }
                    });
                }
            })
            .ok();
        })
        .detach();
    }
    fn start_mirror(&mut self, cx: &mut Context<Self>) {
        if self.scan.is_some() {
            return;
        }
        let Some(choice) = self
            .selected()
            .filter(|device| device.capabilities.is_mirrorable())
        else {
            return;
        };
        let device = choice.device.clone();
        let id = choice.id.clone();
        let audio = self.mirror_audio && choice.capabilities.sdk() >= 30;
        if self.mirrors.contains_key(&id) {
            return;
        }
        let Some(program) = self.scrcpy.clone() else {
            self.error = Some(t!("android.scrcpy_missing"));
            cx.notify();
            return;
        };
        self.error = None;
        self.mirror_id += 1;
        let mirror_id = self.mirror_id;
        let token = Cancellation::new();
        let cancel = token.0.clone();
        self.mirrors.insert(
            id.clone(),
            MirrorState {
                id: mirror_id,
                _cancel: token,
                running: false,
                audio,
            },
        );
        let sender = self.sender.clone();
        let tools = self.tools.clone();
        let failed_id = id.clone();
        let worker = std::thread::Builder::new()
            .name("shelllf-scrcpy".into())
            .spawn(move || {
                let run = || -> Result<()> {
                    let mut process = tools.mirror(&device, &program, audio, &cancel)?;
                    if cancel.load(Ordering::Acquire) {
                        process.stop();
                        return Ok(());
                    }
                    let _ = sender.send(Message::MirrorStarted(id.clone(), mirror_id));
                    loop {
                        if cancel.load(Ordering::Acquire) || device.remote_cancelled() {
                            process.stop();
                            return Ok(());
                        }
                        if !process.poll()? {
                            return Ok(());
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                };
                let result = run().map_err(|error| error.to_string());
                let _ = sender.send(Message::MirrorEnded(id, mirror_id, result));
            });
        match worker {
            Ok(worker) => {
                self.mirror_workers.retain(|worker| !worker.is_finished());
                self.mirror_workers.push(worker);
            }
            Err(error) => {
                self.mirrors.remove(&failed_id);
                self.error = Some(error.to_string().into());
            }
        }
        cx.notify();
    }
    fn receive(&mut self, message: Message, window: &mut Window, cx: &mut Context<Self>) {
        match message {
            Message::Scanned(revision, result) if revision == self.revision => {
                self.scan = None;
                self.clear_apps(window, cx);
                match result {
                    Ok((devices, scrcpy)) => {
                        self.devices = devices;
                        self.selected = self
                            .selected
                            .take()
                            .filter(|id| self.devices.iter().any(|device| &device.id == id));
                        if self.selected.is_none() && self.devices.len() == 1 {
                            self.selected = Some(self.devices[0].id.clone());
                        }
                        let selected = self
                            .selected
                            .as_ref()
                            .and_then(|id| self.devices.iter().position(|device| &device.id == id));
                        self.picker.update(cx, |picker, cx| {
                            picker.set_selected_index(None, window, cx);
                            picker.set_items(self.devices.clone(), window, cx);
                            picker.set_selected_index(
                                selected.map(gpui_kit::component::IndexPath::new),
                                window,
                                cx,
                            );
                            cx.notify();
                        });
                        if self.scrcpy.is_none() {
                            self.scrcpy = scrcpy;
                        }
                        self.mirrors.retain(|serial, _| {
                            self.devices.iter().any(|device| {
                                &device.id == serial && device.capabilities.is_mirrorable()
                            })
                        });
                    }
                    Err(error) => {
                        self.devices.clear();
                        self.selected = None;
                        self.picker.update(cx, |picker, cx| {
                            picker.set_selected_index(None, window, cx);
                            picker.set_items(Vec::new(), window, cx);
                            cx.notify();
                        });
                        self.mirrors.clear();
                        self.error = Some(error.into());
                    }
                }
            }
            Message::Apps(revision, result) if revision == self.revision => {
                self.job = None;
                match result {
                    Ok(apps) => {
                        self.apps.update(cx, |picker, cx| {
                            picker.set_selected_index(None, window, cx);
                            picker.set_items(apps.into_iter().map(AppChoice).collect(), window, cx);
                            cx.notify();
                        });
                        self.status = Some(t!("android.apps_loaded"));
                    }
                    Err(error) => self.error = Some(error.into()),
                }
            }
            Message::Finished(revision, install, result) if revision == self.revision => {
                self.job = None;
                match result {
                    Ok(()) => {
                        self.status = Some(if install {
                            t!("android.installed")
                        } else {
                            t!("android.launched")
                        });
                        if install {
                            self.clear_apps(window, cx);
                        }
                    }
                    Err(error) => self.error = Some(error.into()),
                }
            }
            Message::MirrorStarted(serial, id) => {
                if let Some(state) = self.mirrors.get_mut(&serial).filter(|state| state.id == id) {
                    state.running = true;
                }
            }
            Message::MirrorEnded(serial, id, result) => {
                if self
                    .mirrors
                    .get(&serial)
                    .is_some_and(|state| state.id == id)
                {
                    self.mirrors.remove(&serial);
                    if let Err(error) = result {
                        self.error =
                            Some(t!("android.device_error", device = serial, error = error));
                    }
                }
            }
            _ => return,
        }
        cx.notify();
    }
    pub fn closed(&mut self) {
        self.revision += 1;
        self.job = None;
        self.scan = None;
    }
    fn shutdown(&mut self) -> Vec<std::thread::JoinHandle<()>> {
        self.closed();
        self.mirrors.clear();
        std::mem::take(&mut self.mirror_workers)
    }
}

fn device_label(device: &AdbConnection) -> SharedString {
    if device.display_name() == device.serial() {
        device.serial().to_owned().into()
    } else {
        t!(
            "adb.device_label",
            name = device.display_name(),
            serial = device.serial()
        )
    }
}

impl Render for DeviceManager {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let choice = self.selected();
        let caps = choice.map(|device| device.capabilities).unwrap_or_default();
        let busy = self.job.is_some() || self.scan.is_some();
        let action = |id: &'static str, label: SharedString, command: DeviceCommand| {
            let dispatch = self.dispatch.clone();
            Button::new(id)
                .small()
                .label(label)
                .on_click(move |_, window, cx| {
                    dispatch.dispatch_action(&AdbDeviceAction(command.clone()), window, cx)
                })
        };
        let mirror = self.selected.as_ref().and_then(|id| self.mirrors.get(id));
        let audio = mirror.map_or(self.mirror_audio && caps.sdk() >= 30, |state| state.audio);
        let audio_dispatch = self.dispatch.clone();
        v_flex()
            .id("android-device-manager")
            .test_support()
            .gap_3()
            .w_full()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div().flex_1().min_w_0().child(
                            Select::new(&self.picker)
                                .id("android-device-select")
                                .small()
                                .placeholder(t!("android.select"))
                                .disabled(self.scan.is_some()),
                        ),
                    )
                    .child(
                        action(
                            "android-refresh",
                            t!("android.refresh"),
                            DeviceCommand::Refresh,
                        )
                        .disabled(self.scan.is_some()),
                    ),
            )
            .when(self.scan.is_some(), |view| {
                view.child(
                    div()
                        .id("android-scanning")
                        .test_support()
                        .text_sm()
                        .child(t!("android.scanning")),
                )
            })
            .when_some(choice.filter(|_| self.scan.is_none()), |view, device| {
                view.when(!caps.is_supported(), |view| {
                    let reason = device
                        .problem
                        .clone()
                        .unwrap_or_else(|| t!("android.unsupported").to_string());
                    view.child(
                        div()
                            .id("android-unsupported")
                            .test_support()
                            .aria_label(reason.clone())
                            .text_sm()
                            .child(reason),
                    )
                })
            })
            .when(caps.is_mirrorable() && self.scan.is_none(), |view| {
                let section = v_flex()
                    .gap_2()
                    .child(div().text_sm().child(t!("android.mirror")))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                action(
                                    "android-mirror",
                                    if mirror.is_some() {
                                        t!("android.stop_mirror")
                                    } else {
                                        t!("android.start_mirror")
                                    },
                                    if mirror.is_some() {
                                        DeviceCommand::StopMirror
                                    } else {
                                        DeviceCommand::StartMirror
                                    },
                                )
                                .disabled(mirror.is_none() && self.scrcpy.is_none())
                                .tooltip(t!("android.mirror_note")),
                            )
                            .child(
                                Checkbox::new("android-mirror-audio")
                                    .small()
                                    .label(t!("android.audio"))
                                    .checked(audio)
                                    .disabled(mirror.is_some() || caps.sdk() < 30)
                                    .tooltip(t!("android.audio_note"))
                                    .on_change(move |audio, window, cx| {
                                        audio_dispatch.dispatch_action(
                                            &AdbDeviceAction(DeviceCommand::SetMirrorAudio(*audio)),
                                            window,
                                            cx,
                                        );
                                    }),
                            )
                            .when(self.scrcpy.is_none() || self.error.is_some(), |row| {
                                row.child(
                                    action(
                                        "android-scrcpy-choose",
                                        t!("android.choose_scrcpy"),
                                        DeviceCommand::ChooseScrcpy,
                                    )
                                    .disabled(busy || mirror.is_some()),
                                )
                            }),
                    )
                    .when(self.scrcpy.is_none(), |view| {
                        view.child(
                            div()
                                .id("android-scrcpy-missing")
                                .test_support()
                                .text_sm()
                                .child(t!("android.scrcpy_missing")),
                        )
                    })
                    .when_some(mirror, |view, state| {
                        view.child(
                            div()
                                .id("android-mirror-status")
                                .test_support()
                                .text_sm()
                                .aria_label(if state.running {
                                    t!("android.mirroring")
                                } else {
                                    t!("android.starting")
                                })
                                .child(if state.running {
                                    t!("android.mirroring")
                                } else {
                                    t!("android.starting")
                                }),
                        )
                    });
                view.child(section)
            })
            .when(caps.is_installable() && self.scan.is_none(), |view| {
                view.child(
                    h_flex().child(
                        action(
                            "android-install",
                            t!("android.install"),
                            DeviceCommand::InstallApk,
                        )
                        .disabled(busy),
                    ),
                )
            })
            .when(caps.is_launchable() && self.scan.is_none(), |view| {
                view.child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(t!("android.apps")))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Select::new(&self.apps)
                                            .id("android-app-select")
                                            .small()
                                            .placeholder(t!("android.apps_hint"))
                                            .disabled(busy),
                                    ),
                                )
                                .child(
                                    action(
                                        "android-apps-refresh",
                                        t!("android.load_apps"),
                                        DeviceCommand::RefreshApps,
                                    )
                                    .disabled(busy),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Input::new(&self.package)
                                            .id("android-package")
                                            .small()
                                            .disabled(busy),
                                    ),
                                )
                                .child(
                                    action(
                                        "android-launch",
                                        t!("android.launch"),
                                        DeviceCommand::Launch,
                                    )
                                    .disabled(busy),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("android.package_note")),
                        ),
                )
            })
            .when(self.job.is_some(), |view| {
                view.child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .id("android-working")
                                .test_support()
                                .text_sm()
                                .child(t!("android.working")),
                        )
                        .child(action(
                            "android-cancel",
                            t!("common.cancel"),
                            DeviceCommand::Cancel,
                        )),
                )
            })
            .when_some(self.status.clone(), |view, status| {
                view.child(
                    div()
                        .id("android-result")
                        .test_support()
                        .text_sm()
                        .aria_label(status.clone())
                        .child(status),
                )
            })
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .id("android-error")
                        .test_support()
                        .text_sm()
                        .aria_label(error.clone())
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
    }
}
