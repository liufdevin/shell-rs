use std::{
    collections::{BTreeSet, VecDeque},
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, Instant},
};

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    h_flex,
    menu::PopupMenu,
    progress::Progress,
    radio::Radio,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{CatalogIcon, CenterTab, CloseFlashing, FlashingAction};
use crate::i18n::t;
use crate::settings::SettingsStore;
use crate::shared::{ClosableTabTitle, close_tab_items};

use super::firmware::FirmwareCapabilities;
use super::model::FirmwarePartition;
use super::process::{FirmwareInfo, inspect_firmware};
use super::{
    Completion, EraseMode, FlashProgress, ParsedLine, Phase, Run, RunEvent, adb_devices,
    reboot_to_efex, start_wait, tool_dir,
};

const MAX_LOG_LINES: usize = 4000;

/// The page's commands. Buttons dispatch these to the workspace, which owns
/// the panel and gives them back to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlashingCommand {
    ChooseToolDirectory,
    ChooseFirmware,
    StartViaAdb,
    Stop,
    ClearLog,
    ToggleFollow,
    SetMode(EraseMode),
    SelectPartitionMode,
    TogglePartition(u32),
    SelectAllPartitions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Normal,
    Muted,
    Success,
    Danger,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogLine {
    at: Duration,
    text: String,
    tone: Tone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashingPanelEvent {
    Activated,
    Closed,
}

pub struct FlashingPanel {
    dispatch: FocusHandle,
    settings: Entity<SettingsStore>,
    focus_handle: FocusHandle,
    tool_dir: Option<PathBuf>,
    firmware: Option<PathBuf>,
    mode: EraseMode,
    partition_mode: bool,
    partitions: Vec<FirmwarePartition>,
    selected_partitions: BTreeSet<u32>,
    partition_error: Option<String>,
    capabilities: Option<FirmwareCapabilities>,
    inspection: Option<Run>,
    inspection_receiver: Option<Receiver<Result<FirmwareInfo, String>>>,
    retiring: Option<Run>,
    phase: Phase,
    progress: FlashProgress,
    logs: VecDeque<LogLine>,
    follow: bool,
    scroll: ScrollHandle,
    started: Instant,
    burn_started: Option<Instant>,
    last_duration: Option<Duration>,
    run: Option<Run>,
    receiver: Option<Receiver<RunEvent>>,
    restart_at: Option<Instant>,
    picking: bool,
    listening: bool,
    wait_timed_out: bool,
    adb: Option<Vec<String>>,
    adb_error: Option<String>,
    adb_poll_at: Instant,
    adb_receiver: Option<Receiver<Result<Vec<String>, String>>>,
    adb_trigger: Option<Receiver<Result<String, String>>>,
    last_result: Option<bool>,
    shown_second: u64,
    tab_group: Option<WeakEntity<TabGroup>>,
    _ticks: Task<()>,
}

impl FlashingPanel {
    pub fn new(
        dispatch: FocusHandle,
        settings: Entity<SettingsStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let saved = settings.read(cx).settings();
        let mode = saved.flashing_erase_mode;
        let firmware = saved.flashing_firmware_path.clone();
        let tool_dir = tool_dir(saved.flashing_tool_directory.as_deref());
        let phase = if firmware.is_some() && tool_dir.is_some() {
            Phase::ReadingFirmware
        } else {
            Phase::NoFirmware
        };
        let mut panel = Self {
            dispatch,
            settings,
            focus_handle,
            tool_dir,
            firmware,
            mode,
            partition_mode: saved.flashing_partition_mode,
            partitions: Vec::new(),
            selected_partitions: BTreeSet::new(),
            partition_error: None,
            capabilities: None,
            inspection: None,
            inspection_receiver: None,
            retiring: None,
            phase,
            progress: FlashProgress::default(),
            logs: VecDeque::new(),
            follow: true,
            scroll: ScrollHandle::new(),
            started: Instant::now(),
            burn_started: None,
            last_duration: None,
            run: None,
            receiver: None,
            restart_at: None,
            picking: false,
            listening: false,
            wait_timed_out: false,
            adb: None,
            adb_error: None,
            adb_poll_at: Instant::now(),
            adb_receiver: None,
            adb_trigger: None,
            last_result: None,
            shown_second: 0,
            tab_group: None,
            _ticks: Task::ready(()),
        };
        panel.append(t!("flashing.log.ready"), Tone::Muted, Instant::now());
        if panel.tool_dir.is_none() {
            panel.append(
                t!("flashing.log.tool_missing"),
                Tone::Danger,
                Instant::now(),
            );
        }
        panel._ticks = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                if this.update(cx, |panel, cx| panel.tick(cx)).is_err() {
                    break;
                }
            }
        });
        panel
    }

    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        cx.emit(FlashingPanelEvent::Activated);
        self.adb_poll_at = Instant::now();
        cx.notify();
    }

    pub fn tab_group(&self) -> Option<WeakEntity<TabGroup>> {
        self.tab_group.clone()
    }

    pub(crate) fn is_burning(&self) -> bool {
        self.phase.is_burning()
    }

    pub(crate) fn is_rebooting(&self) -> bool {
        self.adb_trigger.is_some()
    }

    fn running(&self) -> bool {
        self.phase.is_waiting() || self.is_burning()
    }

    fn can_configure(&self) -> bool {
        !self.is_burning()
            && self.inspection.is_none()
            && self.phase != Phase::ReadingFirmware
            && !self.is_rebooting()
            && !self.picking
    }

    fn can_choose_tool(&self) -> bool {
        self.can_configure() && !self.running() && self.run.is_none() && self.retiring.is_none()
    }

    fn supports_mode(&self, mode: EraseMode) -> bool {
        self.capabilities
            .as_ref()
            .is_none_or(|capabilities| match mode {
                EraseMode::KeepData | EraseMode::EraseLogical => true,
                EraseMode::EraseAll => capabilities.full_erase(),
            })
    }

    fn can_wait(&self) -> bool {
        self.can_configure()
            && self.firmware.is_some()
            && self.partition_error.is_none()
            && (!self.partition_mode || !self.selected_partitions.is_empty())
    }

    fn adb_target(&self) -> Option<&str> {
        if !self.listening
            || !self.phase.is_waiting()
            || self.is_rebooting()
            || self.adb_error.is_some()
        {
            return None;
        }
        if let [serial] = self.adb.as_deref()? {
            Some(serial)
        } else {
            None
        }
    }

    pub fn command(&mut self, command: FlashingCommand, cx: &mut Context<Self>) {
        if self.drain_run_events() {
            cx.notify();
        }
        match command {
            FlashingCommand::ChooseToolDirectory => self.choose_tool_directory(cx),
            FlashingCommand::ChooseFirmware => self.choose_firmware(cx),
            FlashingCommand::StartViaAdb => self.start_via_adb(cx),
            FlashingCommand::Stop => self.stop(cx),
            FlashingCommand::ClearLog => {
                self.logs.clear();
                self.append(t!("flashing.log.cleared"), Tone::Muted, Instant::now());
            }
            FlashingCommand::ToggleFollow => {
                self.follow = !self.follow;
                if self.follow {
                    self.scroll.scroll_to_bottom();
                }
            }
            FlashingCommand::SetMode(mode) => {
                if !self.can_configure() || !self.supports_mode(mode) {
                    return;
                }
                self.mode = mode;
                self.partition_mode = false;
                self.settings.update(cx, |store, cx| {
                    store.update(
                        |settings| {
                            settings.flashing_erase_mode = mode;
                            settings.flashing_partition_mode = false;
                        },
                        cx,
                    )
                });
                self.configure_wait();
            }
            FlashingCommand::SelectPartitionMode if self.can_configure() => {
                self.partition_mode = true;
                self.settings.update(cx, |store, cx| {
                    store.update(|settings| settings.flashing_partition_mode = true, cx)
                });
                self.configure_wait();
            }
            FlashingCommand::TogglePartition(index) if self.can_configure() => {
                if self.partitions.iter().any(|part| part.index == index)
                    && !self.selected_partitions.remove(&index)
                {
                    self.selected_partitions.insert(index);
                }
                self.configure_wait();
            }
            FlashingCommand::SelectAllPartitions if self.can_configure() => {
                if self.selected_partitions.len() == self.partitions.len() {
                    self.selected_partitions.clear();
                } else {
                    self.selected_partitions =
                        self.partitions.iter().map(|part| part.index).collect();
                }
                self.configure_wait();
            }
            FlashingCommand::SelectPartitionMode
            | FlashingCommand::TogglePartition(_)
            | FlashingCommand::SelectAllPartitions => {}
        }
        cx.notify();
    }

    fn choose_tool_directory(&mut self, cx: &mut Context<Self>) {
        if !self.can_choose_tool() {
            return;
        }
        self.picking = true;
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(t!("flashing.tool.choose")),
        });
        cx.spawn(async move |panel, cx| {
            let result = picked.await;
            let _ = panel.update(cx, |panel, cx| {
                if !panel.picking {
                    return;
                }
                panel.picking = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            panel.select_tool_directory(path, cx);
                        }
                    }
                    Ok(Err(error)) => panel.append(
                        t!("flashing.log.pick_failed", error = error.to_string()),
                        Tone::Danger,
                        Instant::now(),
                    ),
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_tool_directory(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !super::process::is_tool_dir(&path) {
            self.append(
                t!("flashing.tool.invalid", path = path.display()),
                Tone::Danger,
                Instant::now(),
            );
            return;
        }
        self.tool_dir = Some(path.clone());
        self.partitions.clear();
        self.selected_partitions.clear();
        self.partition_error = None;
        self.capabilities = None;
        self.phase = if self.firmware.is_some() {
            Phase::ReadingFirmware
        } else {
            Phase::NoFirmware
        };
        self.adb = None;
        self.adb_error = None;
        self.adb_receiver = None;
        self.append(
            t!("flashing.tool.ready", path = path.display()),
            Tone::Normal,
            Instant::now(),
        );
        self.settings.update(cx, |store, cx| {
            store.update(|settings| settings.flashing_tool_directory = Some(path), cx);
        });
    }

    fn choose_firmware(&mut self, cx: &mut Context<Self>) {
        if self.tool_dir.is_none() || !self.can_configure() {
            return;
        }
        let was_waiting = self.phase.is_waiting();
        if was_waiting {
            self.retire_listener();
            self.phase = Phase::Ready;
        }
        self.picking = true;
        let picked = super::picker::choose_firmware(self.firmware.clone(), cx);
        cx.spawn(async move |panel, cx| {
            let result = picked.await;
            let _ = panel.update(cx, |panel, cx| {
                if !panel.picking {
                    return;
                }
                panel.picking = false;
                match result {
                    Ok(Some(path)) => panel.select_firmware(path, cx),
                    Err(error) => panel.append(
                        t!("flashing.log.pick_failed", error = error.to_string()),
                        Tone::Danger,
                        Instant::now(),
                    ),
                    _ => {}
                }
                if was_waiting && panel.phase == Phase::Ready {
                    panel.configure_wait();
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_firmware(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !path.is_file()
            || !path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("img"))
        {
            self.append(
                t!("flashing.log.firmware_missing", path = path.display()),
                Tone::Danger,
                Instant::now(),
            );
            return;
        }
        self.retire_listener();
        self.firmware = Some(path.clone());
        self.settings.update(cx, |store, cx| {
            store.update(
                |settings| settings.flashing_firmware_path = Some(path.clone()),
                cx,
            );
        });
        self.partitions.clear();
        self.selected_partitions.clear();
        self.partition_error = None;
        self.capabilities = None;
        self.last_result = None;
        self.last_duration = None;
        self.burn_started = None;
        self.progress.clear();
        self.append(
            t!("flashing.log.firmware_selected", path = path.display()),
            Tone::Normal,
            Instant::now(),
        );
        self.phase = Phase::ReadingFirmware;
        cx.notify();
    }

    fn retire_listener(&mut self) {
        self.stop_process();
        self.receiver = None;
        if let Some(run) = self.run.take() {
            self.retiring = Some(run);
        }
        self.adb = None;
        self.adb_error = None;
        self.adb_receiver = None;
    }

    fn configure_wait(&mut self) {
        self.retire_listener();
        self.progress.clear();
        self.last_result = None;
        self.last_duration = None;
        if self.firmware.is_none() {
            self.phase = Phase::NoFirmware;
        } else if self.can_wait() {
            self.phase = Phase::Waiting;
            self.restart_at = Some(Instant::now());
        } else {
            self.phase = Phase::Ready;
        }
    }

    fn start_wait(&mut self, cx: &mut Context<Self>) {
        if !self.can_wait() || self.retiring.is_some() {
            return;
        }
        let Some(tool_dir) = self.tool_dir.clone() else {
            self.phase = Phase::Failed;
            self.append(
                t!("flashing.log.tool_missing"),
                Tone::Danger,
                Instant::now(),
            );
            cx.notify();
            return;
        };
        let Some(firmware) = self.firmware.clone() else {
            self.phase = Phase::NoFirmware;
            cx.notify();
            return;
        };
        if self.run.is_some() {
            return;
        }
        let rearming = self.phase.is_waiting();
        self.listening = false;
        self.wait_timed_out = false;
        self.adb = None;
        self.adb_error = None;
        self.adb_receiver = None;
        self.phase = Phase::Waiting;
        self.burn_started = None;
        self.adb_poll_at = Instant::now();
        if !rearming {
            self.append(t!("flashing.log.waiting"), Tone::Muted, Instant::now());
        }
        let partitions = self
            .partition_mode
            .then(|| self.selected_partitions.iter().copied().collect());
        let (run, receiver) = start_wait(&tool_dir, &firmware, self.mode, partitions);
        self.run = Some(run);
        self.receiver = Some(receiver);
        cx.notify();
    }

    fn firmware_ready(&mut self, info: FirmwareInfo, cx: &mut Context<Self>) {
        self.partitions = info.partitions;
        self.capabilities = Some(info.capabilities);
        if !self.supports_mode(self.mode) {
            self.mode = EraseMode::KeepData;
            self.settings.update(cx, |store, cx| {
                store.update(
                    |settings| settings.flashing_erase_mode = EraseMode::KeepData,
                    cx,
                )
            });
        }
        self.phase = Phase::Ready;
        self.configure_wait();
    }

    fn stop_process(&mut self) {
        if let Some(run) = &self.run {
            run.stop();
        }
        self.listening = false;
        self.restart_at = None;
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.running() || self.is_rebooting() {
            return;
        }
        self.retire_listener();
        if let Some(started) = self.burn_started.take() {
            self.last_duration = Some(started.elapsed());
        }
        self.phase = Phase::Stopped;
        self.append(t!("flashing.log.stopped"), Tone::Muted, Instant::now());
        cx.notify();
    }

    fn start_via_adb(&mut self, cx: &mut Context<Self>) {
        if self.drain_run_events() {
            cx.notify();
        }
        let (Some(tool_dir), Some(serial)) =
            (self.tool_dir.clone(), self.adb_target().map(str::to_owned))
        else {
            return;
        };
        self.append(
            t!("flashing.log.adb_reboot", serial = serial),
            Tone::Muted,
            Instant::now(),
        );
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(reboot_to_efex(&tool_dir, &serial));
        });
        self.adb_trigger = Some(receiver);
        cx.notify();
    }

    pub(crate) fn drain_run_events(&mut self) -> bool {
        let mut changed = false;
        for _ in 0..512 {
            let Some(receiver) = &self.receiver else {
                break;
            };
            match receiver.try_recv() {
                Ok(event) => self.handle_event(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => self.handle_event(RunEvent::Exited {
                    code: None,
                    stopped: false,
                }),
            }
            changed = true;
        }
        changed
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let mut changed = self.drain_run_events();
        if self.retiring.as_ref().is_some_and(Run::is_finished) {
            self.retiring = None;
            changed = true;
        }
        if self.phase == Phase::ReadingFirmware
            && self.inspection.is_none()
            && self.retiring.is_none()
            && let (Some(tool_dir), Some(firmware)) = (&self.tool_dir, &self.firmware)
        {
            let (inspection, receiver) = inspect_firmware(tool_dir, firmware);
            self.inspection = Some(inspection);
            self.inspection_receiver = Some(receiver);
        }
        if let Some(receiver) = self.inspection_receiver.take() {
            match receiver.try_recv() {
                Ok(result) => {
                    self.inspection = None;
                    changed = true;
                    match result {
                        Ok(info) => self.firmware_ready(info, cx),
                        Err(error) => {
                            self.partition_error = Some(error.clone());
                            self.phase = Phase::Failed;
                            self.append(
                                t!("flashing.partitions.failed", error = error),
                                Tone::Danger,
                                Instant::now(),
                            );
                        }
                    }
                }
                Err(TryRecvError::Empty) => self.inspection_receiver = Some(receiver),
                Err(TryRecvError::Disconnected) => {
                    self.inspection = None;
                    self.partition_error = Some(t!("flashing.partitions.interrupted").to_string());
                    self.phase = Phase::Failed;
                    changed = true;
                }
            }
        }
        if let Some(receiver) = self.adb_trigger.take() {
            match receiver.try_recv() {
                Ok(Ok(output)) => {
                    changed = true;
                    if !output.is_empty() {
                        self.append(output, Tone::Normal, Instant::now());
                    }
                }
                Ok(Err(error)) => {
                    changed = true;
                    self.append(
                        if error.is_empty() {
                            t!("flashing.log.adb_changed")
                        } else {
                            t!("flashing.log.adb_failed", error = error)
                        },
                        Tone::Danger,
                        Instant::now(),
                    );
                }
                Err(TryRecvError::Empty) => self.adb_trigger = Some(receiver),
                Err(TryRecvError::Disconnected) => changed = true,
            }
        }
        if let Some(receiver) = self.adb_receiver.take() {
            match receiver.try_recv() {
                Ok(Ok(devices)) => {
                    changed = true;
                    self.adb = Some(devices);
                    self.adb_error = None;
                }
                Ok(Err(error)) => {
                    changed = true;
                    self.adb = None;
                    self.adb_error = Some(error);
                }
                Err(TryRecvError::Empty) => self.adb_receiver = Some(receiver),
                Err(TryRecvError::Disconnected) => changed = true,
            }
        }
        if self.listening
            && self.phase.is_waiting()
            && self.adb_receiver.is_none()
            && self.adb_poll_at.elapsed() >= Duration::from_secs(3)
        {
            self.adb_poll_at = Instant::now();
            if let Some(tool_dir) = self.tool_dir.clone() {
                let (sender, receiver) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = sender.send(adb_devices(&tool_dir));
                });
                self.adb_receiver = Some(receiver);
            }
        }
        if let Some(at) = self.restart_at
            && Instant::now() >= at
            && self.retiring.is_none()
            && !self.picking
            && !self.is_rebooting()
        {
            self.restart_at = None;
            if self.firmware.is_some() && self.run.is_none() {
                self.start_wait(cx);
                changed = true;
            }
        }
        if let Some(started) = self.burn_started {
            let second = started.elapsed().as_secs();
            if second != self.shown_second {
                self.shown_second = second;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn handle_event(&mut self, event: RunEvent) {
        match event {
            RunEvent::Line(line) => {
                let parsed = super::parse_line(&line);
                match &parsed {
                    ParsedLine::Listening => {
                        if !self.phase.is_waiting() || self.listening {
                            return;
                        }
                        self.listening = true;
                        self.adb_poll_at = Instant::now() - Duration::from_secs(3);
                    }
                    ParsedLine::NoDeviceTimeout if self.phase.is_waiting() => {
                        self.wait_timed_out = true;
                        return;
                    }
                    _ => {}
                }
                if line.contains("Device checkIn time is 30s") && self.phase.is_waiting() {
                    return;
                }
                if matches!(
                    parsed,
                    ParsedLine::DeviceDetected
                        | ParsedLine::Stage(_)
                        | ParsedLine::Progress { .. }
                        | ParsedLine::DeviceFinished { .. }
                ) && self.phase.is_waiting()
                {
                    self.phase = Phase::Burning;
                    self.progress.clear();
                    self.burn_started = Some(Instant::now());
                    self.last_duration = None;
                    self.last_result = None;
                    self.shown_second = 0;
                    self.listening = false;
                    self.wait_timed_out = false;
                    self.append(
                        t!("flashing.log.device_found"),
                        Tone::Success,
                        Instant::now(),
                    );
                }
                let tone = match &parsed {
                    super::ParsedLine::DeviceFinished { success: true, .. }
                    | super::ParsedLine::RunFinished { success: true } => Tone::Success,
                    super::ParsedLine::DeviceFinished { success: false, .. }
                    | super::ParsedLine::RunFinished { success: false } => Tone::Danger,
                    _ => Tone::Normal,
                };
                let finished = match &parsed {
                    ParsedLine::RunFinished { success } if self.phase.is_burning() => {
                        Some(*success)
                    }
                    _ => None,
                };
                self.progress.apply(parsed);
                self.append(line, tone, Instant::now());
                if let Some(success) = finished {
                    self.finish_burn(
                        success && self.progress.completion(Some(0), false) == Completion::Success,
                    );
                }
            }
            RunEvent::Failed(error) => {
                self.run = None;
                self.receiver = None;
                self.listening = false;
                if self.phase == Phase::Stopped {
                    return;
                }
                self.phase = Phase::Failed;
                if let Some(started) = self.burn_started.take() {
                    self.last_duration = Some(started.elapsed());
                }
                self.append(
                    t!("flashing.log.start_failed", error = error),
                    Tone::Danger,
                    Instant::now(),
                );
            }
            RunEvent::Exited { code, stopped } => {
                self.run = None;
                self.receiver = None;
                self.listening = false;
                if stopped || self.phase == Phase::Stopped {
                    if self.phase != Phase::Stopped {
                        self.phase = Phase::Stopped;
                    }
                    return;
                }
                let completion = self
                    .progress
                    .completion(code, self.phase.is_waiting() && self.wait_timed_out);
                if completion == Completion::WaitAgain {
                    self.restart_at = Some(Instant::now() + Duration::from_millis(400));
                    return;
                }
                let success = completion == Completion::Success;
                if success {
                    if self.last_result != Some(true) {
                        self.finish_burn(true);
                    }
                    self.restart_at = Some(Instant::now() + Duration::from_millis(400));
                } else {
                    self.last_result = Some(false);
                    self.phase = Phase::Failed;
                    if let Some(started) = self.burn_started.take() {
                        self.last_duration = Some(started.elapsed());
                    }
                    self.append(
                        t!(
                            "flashing.log.failed",
                            code = code.map_or_else(|| "—".to_string(), |code| code.to_string())
                        ),
                        Tone::Danger,
                        Instant::now(),
                    );
                }
            }
        }
    }

    fn finish_burn(&mut self, success: bool) {
        self.last_duration = self.burn_started.take().map(|started| started.elapsed());
        self.last_result = Some(success);
        self.phase = Phase::Waiting;
        self.listening = true;
        self.wait_timed_out = false;
        self.adb_poll_at = Instant::now() - Duration::from_secs(3);
        self.append(
            if success {
                t!("flashing.log.success")
            } else {
                t!("flashing.result.failed")
            },
            if success { Tone::Success } else { Tone::Danger },
            Instant::now(),
        );
        self.append(t!("flashing.log.next_device"), Tone::Muted, Instant::now());
    }

    fn append(&mut self, text: impl ToString, tone: Tone, now: Instant) {
        let at = now.saturating_duration_since(self.started);
        self.logs.push_back(LogLine {
            at,
            text: text.to_string(),
            tone,
        });
        while self.logs.len() > MAX_LOG_LINES {
            self.logs.pop_front();
        }
        if self.follow {
            self.scroll.scroll_to_bottom();
        }
    }

    fn elapsed(&self) -> String {
        let seconds = self
            .burn_started
            .map(|started| started.elapsed())
            .or(self.last_duration)
            .unwrap_or_default()
            .as_secs();
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }

    fn phase_label(&self) -> SharedString {
        match self.phase {
            Phase::NoFirmware => t!("flashing.phase.no_firmware"),
            Phase::ReadingFirmware => t!("flashing.phase.reading"),
            Phase::Ready => t!("flashing.phase.ready"),
            Phase::Waiting => t!("flashing.phase.waiting"),
            Phase::Burning => t!("flashing.phase.burning"),
            Phase::Failed => t!("flashing.phase.failed"),
            Phase::Stopped => t!("flashing.phase.stopped"),
        }
    }

    fn phase_hint(&self) -> SharedString {
        match self.phase {
            Phase::NoFirmware => t!("flashing.hint.no_firmware"),
            Phase::ReadingFirmware => t!("flashing.hint.reading"),
            Phase::Ready => t!("flashing.partitions.hint"),
            Phase::Waiting => t!("flashing.hint.waiting"),
            Phase::Burning => t!("flashing.hint.burning"),
            Phase::Failed => t!("flashing.hint.failed"),
            Phase::Stopped => t!("flashing.hint.stopped"),
        }
    }

    fn phase_color(&self, cx: &App) -> Hsla {
        match self.phase {
            Phase::Burning => cx.theme().success,
            Phase::Failed => cx.theme().danger,
            Phase::Waiting => cx.theme().primary,
            Phase::NoFirmware | Phase::Stopped | Phase::ReadingFirmware | Phase::Ready => {
                cx.theme().muted_foreground
            }
        }
    }

    fn command_button(
        &self,
        id: &'static str,
        label: SharedString,
        command: FlashingCommand,
        disabled: bool,
    ) -> Button {
        let dispatch = self.dispatch.clone();
        Button::new(id)
            .label(label)
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                dispatch.dispatch_action(&FlashingAction(command.clone()), window, cx)
            })
    }

    fn toolbar(&self) -> impl IntoElement {
        let adb_ready = self.adb_target().is_some();
        let busy = self.running();
        let mode_dispatch = self.dispatch.clone();
        h_flex()
            .gap_2()
            .flex_wrap()
            .child(self.command_button(
                "flashing-choose",
                t!("flashing.choose"),
                FlashingCommand::ChooseFirmware,
                self.tool_dir.is_none() || !self.can_configure(),
            ))
            .child(
                self.command_button(
                    "flashing-start",
                    t!("flashing.start"),
                    FlashingCommand::StartViaAdb,
                    !adb_ready,
                )
                .map(|button| {
                    if adb_ready {
                        button.primary()
                    } else {
                        button.outline()
                    }
                })
                .tooltip(if adb_ready {
                    t!("flashing.start_tooltip")
                } else {
                    t!("flashing.start_unavailable")
                }),
            )
            .when(busy, |row| {
                row.child(self.command_button(
                    "flashing-stop",
                    t!("flashing.stop"),
                    FlashingCommand::Stop,
                    self.is_rebooting(),
                ))
            })
            .child(
                h_flex()
                    .ml_auto()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        Radio::new("flashing-mode-partitions")
                            .label(t!("flashing.mode.partitions"))
                            .small()
                            .checked(self.partition_mode)
                            .disabled(!self.can_configure())
                            .on_click({
                                let dispatch = mode_dispatch.clone();
                                move |_, window, cx| {
                                    dispatch.dispatch_action(
                                        &FlashingAction(FlashingCommand::SelectPartitionMode),
                                        window,
                                        cx,
                                    );
                                }
                            }),
                    )
                    .children(EraseMode::ALL.into_iter().map(|mode| {
                        let dispatch = mode_dispatch.clone();
                        Radio::new(SharedString::from(format!("flashing-mode-{}", mode.flag())))
                            .small()
                            .label(match mode {
                                EraseMode::KeepData => t!("flashing.mode.keep"),
                                EraseMode::EraseLogical => t!("flashing.mode.logical"),
                                EraseMode::EraseAll => t!("flashing.mode.all"),
                            })
                            .checked(!self.partition_mode && self.mode == mode)
                            .disabled(!self.can_configure() || !self.supports_mode(mode))
                            .when(!self.supports_mode(mode), |button| {
                                button.tooltip(t!("flashing.mode.all_unavailable"))
                            })
                            .on_click(move |_, window, cx| {
                                dispatch.dispatch_action(
                                    &FlashingAction(FlashingCommand::SetMode(mode)),
                                    window,
                                    cx,
                                )
                            })
                    })),
            )
    }

    fn partitions_card(&self) -> impl IntoElement {
        let dispatch = self.dispatch.clone();
        let configurable = self.can_configure();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Checkbox::new("flashing-partitions-all")
                            .small()
                            .label(t!("flashing.partitions.all"))
                            .checked(
                                !self.partitions.is_empty()
                                    && self.selected_partitions.len() == self.partitions.len(),
                            )
                            .disabled(!configurable || self.partitions.is_empty())
                            .on_click(move |_, window, cx| {
                                dispatch.dispatch_action(
                                    &FlashingAction(FlashingCommand::SelectAllPartitions),
                                    window,
                                    cx,
                                );
                            }),
                    )
                    .child(div().text_sm().child(t!(
                        "flashing.partitions.selected",
                        selected = self.selected_partitions.len(),
                        total = self.partitions.len()
                    ))),
            )
            .child(
                v_flex()
                    .id("flashing-partitions")
                    .test_support()
                    .max_h_40()
                    .overflow_y_scroll()
                    .gap_1()
                    .children(self.partitions.iter().map(|partition| {
                        let dispatch = self.dispatch.clone();
                        let index = partition.index;
                        Checkbox::new(SharedString::from(format!("flashing-partition-{index}")))
                            .small()
                            .label(format!("{} · {}", index, partition.name))
                            .checked(self.selected_partitions.contains(&index))
                            .disabled(!configurable)
                            .on_click(move |_, window, cx| {
                                dispatch.dispatch_action(
                                    &FlashingAction(FlashingCommand::TogglePartition(index)),
                                    window,
                                    cx,
                                );
                            })
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .child(if self.phase == Phase::ReadingFirmware {
                        t!("flashing.hint.reading")
                    } else if self.partitions.is_empty() {
                        t!("flashing.partitions.empty")
                    } else {
                        t!("flashing.partitions.hint")
                    }),
            )
    }

    fn progress_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let percent = self.progress.overall_percent();
        let color = self.phase_color(cx);
        v_flex()
            .gap_2()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded_full()
                            .bg(color.opacity(0.14))
                            .text_color(color)
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.phase_label()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.phase_hint()),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .when(
                                self.burn_started.is_some() || self.last_duration.is_some(),
                                |label| {
                                    let text = if self.burn_started.is_some() {
                                        t!("flashing.elapsed", time = self.elapsed())
                                    } else {
                                        t!("flashing.last_elapsed", time = self.elapsed())
                                    };
                                    label.child(
                                        div()
                                            .id("flashing-elapsed")
                                            .test_support()
                                            .aria_label(text.clone())
                                            .child(text),
                                    )
                                },
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(t!("flashing.overall")),
                    )
                    .child(
                        div()
                            .text_lg()
                            .text_color(color)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(
                                percent
                                    .map(|percent| format!("{percent}%"))
                                    .unwrap_or_else(|| "—".into()),
                            ),
                    ),
            )
            .child(
                Progress::new("flashing-overall")
                    .accessibility_label(t!("flashing.overall"))
                    .rounded_full()
                    .color(color)
                    .loading(self.is_burning() && percent.is_none())
                    .value(percent.map_or(0., f32::from)),
            )
            .when_some(self.progress.stage().map(str::to_string), |card, stage| {
                card.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("flashing.stage", stage = stage)),
                )
            })
            .when(!self.progress.is_empty(), |card| {
                card.child(v_flex().gap_1p5().children(self.progress.devices().map(
                    |(device, state)| {
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .w_16()
                                    .text_xs()
                                    .child(t!("flashing.device", number = device)),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    Progress::new(SharedString::from(format!(
                                        "flashing-device-{device}"
                                    )))
                                    .xsmall()
                                    .value(f32::from(state.percent))
                                    .color(
                                        if state.finished && !state.success {
                                            cx.theme().danger
                                        } else {
                                            cx.theme().success
                                        },
                                    ),
                                ),
                            )
                            .child(
                                div()
                                    .w_10()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{}%", state.percent)),
                            )
                            .child(
                                div()
                                    .max_w_40()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .children(state.stage.clone()),
                            )
                    },
                )))
            })
            .when_some(self.last_result, |card, success| {
                card.child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if success {
                            cx.theme().success
                        } else {
                            cx.theme().danger
                        })
                        .child(if success {
                            t!("flashing.result.success")
                        } else {
                            t!("flashing.result.failed")
                        }),
                )
            })
    }

    fn log_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let dispatch = self.dispatch.clone();
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(t!("flashing.log.title")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("flashing.log.lines", count = self.logs.len())),
                    )
                    .child(
                        h_flex()
                            .ml_auto()
                            .gap_2()
                            .child(
                                Checkbox::new("flashing-follow")
                                    .small()
                                    .label(t!("flashing.log.follow"))
                                    .checked(self.follow)
                                    .on_click({
                                        let dispatch = dispatch.clone();
                                        move |_, window, cx| {
                                            dispatch.dispatch_action(
                                                &FlashingAction(FlashingCommand::ToggleFollow),
                                                window,
                                                cx,
                                            )
                                        }
                                    }),
                            )
                            .child(
                                Button::new("flashing-clear-log")
                                    .small()
                                    .ghost()
                                    .label(t!("flashing.log.clear"))
                                    .on_click({
                                        let dispatch = dispatch.clone();
                                        move |_, window, cx| {
                                            dispatch.dispatch_action(
                                                &FlashingAction(FlashingCommand::ClearLog),
                                                window,
                                                cx,
                                            )
                                        }
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .id("flashing-log")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_sm()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(self.logs.iter().map(|line| {
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(
                                div()
                                    .w_10()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{:02}:{:02}",
                                        line.at.as_secs() / 60,
                                        line.at.as_secs() % 60
                                    )),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .text_color(match line.tone {
                                        Tone::Normal => cx.theme().foreground,
                                        Tone::Muted => cx.theme().muted_foreground,
                                        Tone::Success => cx.theme().success,
                                        Tone::Danger => cx.theme().danger,
                                    })
                                    .child(line.text.clone()),
                            )
                    })),
            )
    }
}

impl EventEmitter<PanelEvent> for FlashingPanel {}
impl EventEmitter<FlashingPanelEvent> for FlashingPanel {}

impl Focusable for FlashingPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for FlashingPanel {
    fn panel_name(&self) -> &'static str {
        "FlashingPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            self.show(window, cx);
        }
    }

    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.tab_group = Some(group);
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.tab_group = None;
        if crate::shared::is_panel_transfer(cx.entity_id(), cx) {
            return;
        }
        self.picking = false;
        self.phase = Phase::Stopped;
        self.stop_process();
        self.inspection = None;
        self.inspection_receiver = None;
        cx.emit(FlashingPanelEvent::Closed);
    }
}

impl Panel for FlashingPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (group, panel) = (self.tab_group.clone(), cx.entity_id());
        ClosableTabTitle::new("flashing-tab", super::FlashingMark, t!("flashing.title"))
            .closable("close-flashing", Box::new(CloseFlashing))
            .context_menu(move |menu, _, cx| tab_menu(menu, group.clone(), panel, cx))
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        tab_menu(menu, self.tab_group.clone(), cx.entity_id(), cx)
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for FlashingPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tool_state = if let Some(dir) = &self.tool_dir {
            t!("flashing.tool.ready", path = dir.display())
        } else {
            t!("flashing.tool.missing")
        };
        let firmware = self
            .firmware
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| t!("flashing.no_firmware").to_string());
        let adb_state = match (&self.adb, &self.adb_error) {
            (_, Some(error)) => t!("flashing.adb.error", error = error),
            (Some(devices), _) if devices.len() > 1 => t!("flashing.adb.multiple"),
            (Some(devices), _) if devices.len() == 1 => {
                t!("flashing.adb.ready", serial = devices.join(", "))
            }
            (Some(_), _) => t!("flashing.adb.empty"),
            (None, _) if self.listening => t!("flashing.adb.checking"),
            (None, _) => t!("flashing.adb.idle"),
        };
        v_flex()
            .id("flashing")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .p_4()
            .gap_3()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(t!("flashing.title")),
                    )
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .id("flashing-tool-directory")
                                    .test_support()
                                    .aria_label(tool_state.clone())
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(tool_state),
                            )
                            .child(
                                self.command_button(
                                    "flashing-choose-tool",
                                    t!("flashing.tool.choose"),
                                    FlashingCommand::ChooseToolDirectory,
                                    !self.can_choose_tool(),
                                )
                                .small()
                                .outline()
                                .icon(CatalogIcon::FolderRoot),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .w_16()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("flashing.firmware")),
                            )
                            .child(
                                div()
                                    .id("flashing-firmware-path")
                                    .test_support()
                                    .aria_label(firmware.clone())
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_sm()
                                    .child(firmware),
                            ),
                    )
                    .when(
                        self.listening || self.adb.is_some() || self.adb_error.is_some(),
                        |card| {
                            card.child(
                                h_flex()
                                    .id("flashing-adb")
                                    .test_support()
                                    .gap_2()
                                    .child(
                                        div()
                                            .w_16()
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(t!("flashing.adb.label")),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .text_color(if self.adb_error.is_some() {
                                                cx.theme().danger
                                            } else {
                                                cx.theme().muted_foreground
                                            })
                                            .child(adb_state),
                                    ),
                            )
                        },
                    )
                    .child(self.toolbar()),
            )
            .when(self.partition_mode, |panel| {
                panel.child(self.partitions_card())
            })
            .child(self.progress_card(cx))
            .child(self.log_card(cx))
    }
}

fn tab_menu(
    menu: PopupMenu,
    group: Option<WeakEntity<TabGroup>>,
    panel: EntityId,
    cx: &App,
) -> PopupMenu {
    close_tab_items(menu, CenterTab::Flashing, group, panel, cx)
}

#[cfg(test)]
mod tests {
    use gpui_kit::{AppContext as _, TestAppContext};

    use super::{FlashingPanel, Phase, RunEvent};

    #[gpui_kit::test]
    fn a_completed_burn_rearms_while_a_failed_burn_stops(cx: &mut TestAppContext) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, _| {
            panel.phase = Phase::Waiting;
            panel.handle_event(RunEvent::Line("line[42], device:[1] fes plugin".into()));
            assert_eq!(panel.phase, Phase::Burning);
            panel.handle_event(RunEvent::Line("line[43], device [1] progress = 45%".into()));
            assert_eq!(panel.progress.overall_percent(), Some(45));
            panel.handle_event(RunEvent::Line("line[44], device burn result=1".into()));
            panel.handle_event(RunEvent::Exited {
                code: Some(0),
                stopped: false,
            });
            assert_eq!(panel.phase, Phase::Waiting);
            assert_eq!(panel.last_result, Some(true));
            assert!(panel.restart_at.take().is_some());
            assert_eq!(panel.progress.overall_percent(), Some(100));

            panel.handle_event(RunEvent::Line("line[42], device:[1] fes plugin".into()));
            panel.handle_event(RunEvent::Line("line[44], device burn result=0".into()));
            panel.handle_event(RunEvent::Exited {
                code: Some(0),
                stopped: false,
            });
            assert_eq!(panel.phase, Phase::Failed);
            assert_eq!(panel.last_result, Some(false));
            assert!(panel.restart_at.is_none());

            panel.phase = Phase::Stopped;
            panel.handle_event(RunEvent::Line("line[44], device burn result=1".into()));
            panel.handle_event(RunEvent::Exited {
                code: Some(0),
                stopped: false,
            });
            assert_eq!(panel.phase, Phase::Stopped);
            assert!(panel.restart_at.is_none());
        });
    }

    #[gpui_kit::test]
    fn idle_listening_is_quiet_and_only_device_writes_start_the_timer(cx: &mut TestAppContext) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, _| {
            panel.phase = Phase::Waiting;
            panel.logs.clear();
            for _ in 0..200 {
                panel.handle_event(RunEvent::Line(
                    "line[1591], usbproproc service is ready, listening device...".into(),
                ));
            }
            assert_eq!(panel.logs.len(), 1);
            assert!(panel.burn_started.is_none());
            panel.handle_event(RunEvent::Line(
                "line[50], error: no device checkIn in 30 senconds, will end this process!".into(),
            ));
            assert_eq!(panel.logs.len(), 1);
            panel.handle_event(RunEvent::Exited {
                code: Some(1),
                stopped: false,
            });
            assert_eq!(panel.phase, Phase::Waiting);
            assert!(panel.restart_at.is_some());
            assert!(panel.last_result.is_none());

            panel.restart_at = None;
            panel.handle_event(RunEvent::Line("device:[1] fes plugin".into()));
            assert_eq!(panel.phase, Phase::Burning);
            assert!(panel.burn_started.is_some());
            panel.burn_started =
                Some(std::time::Instant::now() - std::time::Duration::from_secs(8));
            panel.handle_event(RunEvent::Line("device [1] progress = 100%".into()));
            panel.handle_event(RunEvent::Line("device burn result=1".into()));
            assert_eq!(panel.phase, Phase::Waiting);
            assert!(panel.burn_started.is_none());
            assert_eq!(panel.elapsed(), "00:08");
            assert_eq!(panel.last_result, Some(true));
            assert!(panel.restart_at.is_none());
            panel.handle_event(RunEvent::Line("device:[1] fes plugin".into()));
            assert_eq!(panel.phase, Phase::Burning);
            assert!(panel.burn_started.is_some());
            assert!(panel.progress.is_empty());
            assert!(panel.last_result.is_none());
        });
    }

    #[gpui_kit::test]
    fn partitions_remain_editable_while_waiting_and_lock_only_during_writes(
        cx: &mut TestAppContext,
    ) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, cx| {
            panel.partition_mode = true;
            panel.firmware = Some(std::path::PathBuf::from("firmware.img"));
            panel.partitions = vec![
                super::FirmwarePartition {
                    index: 1,
                    name: "BOOT".into(),
                },
                super::FirmwarePartition {
                    index: 2,
                    name: "ROOTFS".into(),
                },
            ];
            panel.phase = Phase::Ready;
            assert!(!panel.can_wait());
            panel.command(super::FlashingCommand::TogglePartition(88), cx);
            assert!(panel.selected_partitions.is_empty());
            panel.command(super::FlashingCommand::TogglePartition(1), cx);
            assert!(panel.can_wait());
            panel.command(super::FlashingCommand::SelectAllPartitions, cx);
            assert_eq!(
                panel
                    .selected_partitions
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
                [1, 2]
            );
            panel.phase = Phase::Waiting;
            panel.command(super::FlashingCommand::TogglePartition(1), cx);
            assert_eq!(
                panel
                    .selected_partitions
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
                [2]
            );
            assert_eq!(panel.phase, Phase::Waiting);
            assert!(panel.restart_at.is_some());
            panel.phase = Phase::Burning;
            panel.command(super::FlashingCommand::TogglePartition(2), cx);
            assert!(panel.selected_partitions.contains(&2));
            panel.phase = Phase::Ready;
            panel.command(super::FlashingCommand::SelectAllPartitions, cx);
            panel.command(super::FlashingCommand::SelectAllPartitions, cx);
            assert!(!panel.can_wait());
            assert_eq!(panel.phase, Phase::Ready);
            assert!(panel.restart_at.is_none());
        });
    }

    #[gpui_kit::test]
    fn waiting_allows_mode_changes_but_unsupported_erase_is_rejected(cx: &mut TestAppContext) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings.clone(), cx));
        panel.update(cx, |panel, cx| {
            panel.firmware = Some(std::path::PathBuf::from("firmware.img"));
            panel.phase = Phase::Waiting;
            panel.capabilities = Some(super::FirmwareCapabilities {
                plugin_product_version: None,
            });
            assert!(panel.can_configure());
            panel.command(
                super::FlashingCommand::SetMode(super::EraseMode::EraseAll),
                cx,
            );
            assert_eq!(panel.mode, super::EraseMode::KeepData);
            panel.command(
                super::FlashingCommand::SetMode(super::EraseMode::EraseLogical),
                cx,
            );
            assert_eq!(panel.mode, super::EraseMode::EraseLogical);
            assert_eq!(panel.phase, Phase::Waiting);
            assert!(panel.restart_at.is_some());
            assert!(panel.burn_started.is_none());
            assert_eq!(
                settings.read(cx).settings().flashing_erase_mode,
                super::EraseMode::EraseLogical
            );
            panel.stop(cx);
            assert_eq!(panel.phase, Phase::Stopped);
            assert!(panel.restart_at.is_none());
            panel.command(
                super::FlashingCommand::SetMode(super::EraseMode::EraseLogical),
                cx,
            );
            assert_eq!(panel.phase, Phase::Waiting);
            panel.phase = Phase::Burning;
            panel.command(
                super::FlashingCommand::SetMode(super::EraseMode::KeepData),
                cx,
            );
            assert_eq!(panel.mode, super::EraseMode::EraseLogical);
            assert!(!panel.can_configure());
        });
    }

    #[gpui_kit::test]
    fn changing_configuration_discards_old_events_and_drains_write_events_first(
        cx: &mut TestAppContext,
    ) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, cx| {
            panel.phase = Phase::Waiting;
            panel.firmware = Some(std::path::PathBuf::from("firmware.img"));
            let (sender, receiver) = std::sync::mpsc::channel();
            panel.receiver = Some(receiver);
            panel.command(
                super::FlashingCommand::SetMode(super::EraseMode::EraseLogical),
                cx,
            );
            assert!(
                sender
                    .send(RunEvent::Exited {
                        code: Some(1),
                        stopped: true
                    })
                    .is_err()
            );
            assert_eq!(panel.phase, Phase::Waiting);
            assert!(panel.restart_at.is_some());

            let (sender, receiver) = std::sync::mpsc::channel();
            panel.receiver = Some(receiver);
            sender
                .send(RunEvent::Line("device:[1] fes plugin".into()))
                .unwrap();
            panel.command(
                super::FlashingCommand::SetMode(super::EraseMode::KeepData),
                cx,
            );
            assert_eq!(panel.phase, Phase::Burning);
            assert_eq!(panel.mode, super::EraseMode::EraseLogical);
        });
    }

    #[gpui_kit::test]
    fn replacing_firmware_clears_partitions_and_saves_a_compatible_erase_mode(
        cx: &mut TestAppContext,
    ) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings.clone(), cx));
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("replacement.img");
        std::fs::write(&path, b"fixture").unwrap();
        panel.update(cx, |panel, cx| {
            panel.phase = Phase::Waiting;
            panel.mode = super::EraseMode::EraseAll;
            panel.selected_partitions.insert(1);
            let (sender, receiver) = std::sync::mpsc::channel();
            panel.receiver = Some(receiver);
            panel.select_firmware(path.clone(), cx);
            assert_eq!(panel.firmware.as_ref(), Some(&path));
            assert_eq!(panel.phase, Phase::ReadingFirmware);
            assert!(!panel.can_configure());
            assert!(panel.selected_partitions.is_empty());
            assert!(
                sender
                    .send(RunEvent::Line("device:[1] fes plugin".into()))
                    .is_err()
            );
            panel.firmware_ready(
                super::FirmwareInfo {
                    partitions: vec![],
                    capabilities: super::FirmwareCapabilities::default(),
                },
                cx,
            );
            assert_eq!(panel.mode, super::EraseMode::KeepData);
            assert_eq!(
                settings.read(cx).settings().flashing_erase_mode,
                super::EraseMode::KeepData
            );
            assert_eq!(panel.phase, Phase::Waiting);
            assert!(panel.can_configure());
            assert!(panel.burn_started.is_none());
        });
    }

    #[gpui_kit::test]
    fn selected_firmware_is_saved_and_restored_when_the_panel_is_recreated(
        cx: &mut TestAppContext,
    ) {
        let folder = tempfile::tempdir().unwrap();
        let firmware = folder.path().join("board.IMG");
        std::fs::write(&firmware, b"fixture").unwrap();
        let invalid = folder.path().join("board.txt");
        std::fs::write(&invalid, b"fixture").unwrap();
        let tools = folder.path().join("tools");
        std::fs::create_dir(&tools).unwrap();
        std::fs::write(tools.join("PhoenixConsole.exe"), []).unwrap();
        std::fs::write(tools.join("adb.exe"), []).unwrap();
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings.clone(), cx));
        panel.update(cx, |panel, cx| {
            panel.select_firmware(firmware.clone(), cx);
            panel.select_firmware(invalid, cx);
            assert_eq!(panel.firmware.as_ref(), Some(&firmware));
            assert_eq!(
                settings.read(cx).settings().flashing_firmware_path,
                Some(firmware.clone())
            );
            panel.select_tool_directory(tools.clone(), cx);
            assert_eq!(panel.firmware.as_ref(), Some(&firmware));
            assert_eq!(panel.phase, Phase::ReadingFirmware);
        });
        let reopened = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        reopened.read_with(cx, |panel, _| {
            assert_eq!(panel.firmware.as_ref(), Some(&firmware));
            assert_eq!(panel.tool_dir.as_ref(), Some(&tools));
            assert_eq!(panel.phase, Phase::ReadingFirmware);
            assert!(panel.inspection.is_none());
            assert!(panel.run.is_none());
            assert!(panel.burn_started.is_none());
        });
    }
}
