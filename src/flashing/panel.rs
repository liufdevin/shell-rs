use std::{
    collections::{BTreeSet, VecDeque},
    path::PathBuf,
    sync::mpsc::{Receiver, TryRecvError},
    time::{Duration, Instant},
};

use gpui_kit::base::SelectableText;
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
use super::model::{DeviceProgress, FirmwarePartition};
use super::process::{AdbReboot, FirmwareInfo, inspect_firmware, start_adb_reboot};
use super::source::{self, Observation, SourceState};
use super::{
    Completion, EraseMode, FlashProgress, ParsedLine, Phase, Run, RunEvent, start_wait, tool_dir,
};

const MAX_LOG_LINES: usize = 4000;
const DEVICE_CLAIM_NOTICE: Duration = Duration::from_secs(15);

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
    source: SourceState,
    source_probe: Option<Receiver<Observation>>,
    next_source_probe: Instant,
    source_reload_pending: bool,
    source_retry_at: Option<Instant>,
    source_partition_names: Option<BTreeSet<String>>,
    retiring: Option<Run>,
    phase: Phase,
    progress: FlashProgress,
    logs: VecDeque<LogLine>,
    follow: bool,
    scroll: ScrollHandle,
    started: Instant,
    burn_started: Option<Instant>,
    burn_log_started: Option<Duration>,
    last_duration: Option<Duration>,
    run: Option<Run>,
    receiver: Option<Receiver<RunEvent>>,
    restart_at: Option<Instant>,
    picking: bool,
    listening: bool,
    wait_timed_out: bool,
    fel_devices: BTreeSet<String>,
    device_lost_at: Option<Instant>,
    closed: bool,
    adb_run: Option<Run>,
    adb_trigger: Option<Receiver<Result<AdbReboot, String>>>,
    last_result: Option<bool>,
    tab_active: bool,
    unread_output: bool,
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
            source: SourceState::default(),
            source_probe: None,
            next_source_probe: Instant::now(),
            source_reload_pending: false,
            source_retry_at: None,
            source_partition_names: None,
            retiring: None,
            phase,
            progress: FlashProgress::default(),
            logs: VecDeque::new(),
            follow: true,
            scroll: ScrollHandle::new(),
            started: Instant::now(),
            burn_started: None,
            burn_log_started: None,
            last_duration: None,
            run: None,
            receiver: None,
            restart_at: None,
            picking: false,
            listening: false,
            wait_timed_out: false,
            fel_devices: BTreeSet::new(),
            device_lost_at: None,
            closed: false,
            adb_run: None,
            adb_trigger: None,
            last_result: None,
            tab_active: false,
            unread_output: false,
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
        self.tab_active = true;
        self.unread_output = false;
        window.focus(&self.focus_handle, cx);
        cx.emit(FlashingPanelEvent::Activated);
        cx.notify();
    }

    pub fn tab_group(&self) -> Option<WeakEntity<TabGroup>> {
        self.tab_group.clone()
    }

    pub(crate) fn is_burning(&self) -> bool {
        self.phase.is_burning()
    }

    pub(crate) fn has_unread_output(&self) -> bool {
        self.unread_output
    }

    pub(crate) fn is_rebooting(&self) -> bool {
        self.adb_trigger.is_some()
    }

    fn running(&self) -> bool {
        self.phase.is_waiting()
            || self.is_burning()
            || self.source_retry_at.is_some()
            || matches!(
                self.phase,
                Phase::Unclaimed | Phase::FirmwareMissing | Phase::FirmwareUpdating
            )
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
            && !matches!(self.phase, Phase::FirmwareMissing | Phase::FirmwareUpdating)
            && !self.source_reload_pending
            && self.firmware.is_some()
            && self.partition_error.is_none()
            && (!self.partition_mode || !self.selected_partitions.is_empty())
    }

    fn can_reboot(&self) -> bool {
        self.listening && self.phase.is_waiting() && !self.is_rebooting()
    }

    pub fn command(&mut self, command: FlashingCommand, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
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
        if !path
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
        self.source = SourceState::default();
        self.source_probe = None;
        self.next_source_probe = Instant::now();
        self.source_reload_pending = false;
        self.source_retry_at = None;
        self.source_partition_names = None;
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
        self.burn_log_started = None;
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
        self.fel_devices.clear();
        self.device_lost_at = None;
        self.receiver = None;
        if let Some(run) = self.run.take() {
            self.retiring = Some(run);
        }
    }

    fn configure_wait(&mut self) {
        if self.source_partition_names.is_some() {
            self.source_partition_names = Some(
                self.partitions
                    .iter()
                    .filter(|part| self.selected_partitions.contains(&part.index))
                    .map(|part| part.name.clone())
                    .collect(),
            );
        }
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
        if !self.can_wait() || self.retiring.is_some() || !self.source.ready() {
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
        self.phase = Phase::Waiting;
        self.burn_started = None;
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
        self.partition_error = None;
        self.source_retry_at = None;
        if let Some(names) = self.source_partition_names.take() {
            let selected: BTreeSet<_> = self
                .partitions
                .iter()
                .filter(|part| names.contains(&part.name))
                .map(|part| part.index)
                .collect();
            if selected.len() == names.len() {
                self.selected_partitions = selected;
            } else {
                self.selected_partitions.clear();
                self.append(
                    t!("flashing.source.partitions_changed"),
                    Tone::Muted,
                    Instant::now(),
                );
            }
        }
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

    fn shutdown(&mut self) {
        self.closed = true;
        self.picking = false;
        self.phase = Phase::Stopped;
        self.stop_process();
        self.receiver = None;
        self.inspection_receiver = None;
        self.adb_trigger = None;
        self.source_probe = None;
        self.source_reload_pending = false;
        self.source_retry_at = None;
        self.adb_run = None;
        self.run = None;
        self.inspection = None;
        self.retiring = None;
        self.burn_started = None;
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.running() || self.is_rebooting() {
            return;
        }
        self.retire_listener();
        self.source_probe = None;
        self.source_retry_at = None;
        self.progress.interrupt_active(Instant::now());
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
        if !self.can_reboot() {
            return;
        }
        self.append(t!("flashing.log.adb_checking"), Tone::Muted, Instant::now());
        let (run, receiver) = start_adb_reboot();
        self.adb_run = Some(run);
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
                    at: Instant::now(),
                    code: None,
                    stopped: false,
                }),
            }
            changed = true;
        }
        changed
    }

    fn update_source(&mut self, observation: Observation, at: Instant) -> bool {
        let initial = self.source.observation().is_none();
        if !self.source.update(observation, at) {
            return false;
        }
        if !initial || !self.source.ready() {
            if self.source_partition_names.is_none() {
                self.source_partition_names = Some(
                    self.partitions
                        .iter()
                        .filter(|part| self.selected_partitions.contains(&part.index))
                        .map(|part| part.name.clone())
                        .collect(),
                );
            }
            self.source_reload_pending = true;
        }
        true
    }

    fn poll_source(&mut self) -> bool {
        if self.phase == Phase::Stopped || self.picking {
            return false;
        }
        let mut changed = false;
        if let Some(receiver) = self.source_probe.take() {
            match receiver.try_recv() {
                Ok(observation) => changed = self.update_source(observation, Instant::now()),
                Err(TryRecvError::Empty) => self.source_probe = Some(receiver),
                Err(TryRecvError::Disconnected) => {
                    changed = self.update_source(
                        Observation::Unavailable(
                            t!("flashing.source.check_interrupted").to_string(),
                        ),
                        Instant::now(),
                    );
                }
            }
        }
        if self.source_probe.is_none()
            && Instant::now() >= self.next_source_probe
            && let Some(path) = &self.firmware
        {
            self.source_probe = Some(source::probe(path.clone()));
            self.next_source_probe = Instant::now() + source::CHECK_INTERVAL;
        }
        changed
    }

    fn apply_source_reload(&mut self) -> bool {
        if !self.source_reload_pending
            || self.progress.has_active()
            || self.is_rebooting()
            || self.phase == Phase::Stopped
        {
            return false;
        }
        let phase = if self.source.ready() {
            if self.tool_dir.is_some() {
                Phase::ReadingFirmware
            } else {
                Phase::NoFirmware
            }
        } else if matches!(
            self.source.observation(),
            Some(Observation::Missing | Observation::Unavailable(_))
        ) {
            Phase::FirmwareMissing
        } else {
            Phase::FirmwareUpdating
        };
        let changed = self.phase != phase;
        if self.source.ready() {
            self.source_reload_pending = false;
        }
        if changed || self.run.is_some() || self.inspection.is_some() {
            self.retire_listener();
            if let Some(inspection) = self.inspection.take() {
                inspection.stop();
                self.retiring = Some(inspection);
            }
            self.inspection_receiver = None;
            self.phase = phase;
            self.burn_started = None;
            self.source_retry_at = None;
            if changed {
                if let Some(message) = self.source_hint() {
                    self.append(message, Tone::Muted, Instant::now());
                } else {
                    self.append(t!("flashing.source.restored"), Tone::Muted, Instant::now());
                }
            }
        }
        changed
    }

    fn source_hint(&self) -> Option<SharedString> {
        match self.source.observation() {
            Some(Observation::Missing) => Some(t!("flashing.source.missing")),
            Some(Observation::Unavailable(error)) => {
                Some(t!("flashing.source.unavailable", error = error))
            }
            Some(Observation::Present(_)) if !self.source.ready() => {
                Some(t!("flashing.source.updating"))
            }
            _ if self.source_reload_pending => Some(t!("flashing.source.pending")),
            _ => None,
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        let mut changed = self.drain_run_events();
        changed |= self.poll_source();
        changed |= self.apply_source_reload();
        if matches!(self.phase, Phase::Preparing | Phase::Unclaimed)
            && self.fel_devices.is_empty()
            && self
                .device_lost_at
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(2))
        {
            self.phase = Phase::Waiting;
            self.burn_started = None;
            self.burn_log_started = None;
            self.device_lost_at = None;
            self.append(
                t!("flashing.log.device_removed"),
                Tone::Muted,
                Instant::now(),
            );
            changed = true;
        }
        if self.phase == Phase::Preparing
            && self
                .burn_started
                .is_some_and(|at| at.elapsed() >= DEVICE_CLAIM_NOTICE)
        {
            self.phase = Phase::Unclaimed;
            self.append(
                t!("flashing.log.device_unclaimed"),
                Tone::Muted,
                Instant::now(),
            );
            changed = true;
        }
        if self.retiring.as_ref().is_some_and(Run::is_finished) {
            self.retiring = None;
            changed = true;
        }
        if self.source_retry_at.is_some_and(|at| Instant::now() >= at)
            && self.source.ready()
            && !self.source_reload_pending
            && self.phase == Phase::Failed
        {
            self.source_retry_at = None;
            self.phase = Phase::ReadingFirmware;
            changed = true;
        }
        if self.phase == Phase::ReadingFirmware
            && self.inspection.is_none()
            && self.retiring.is_none()
            && self.source.ready()
            && self.source_probe.is_none()
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
                            let repeated = self.partition_error.as_ref() == Some(&error);
                            self.partition_error = Some(error.clone());
                            self.phase = Phase::Failed;
                            self.source_retry_at = Some(Instant::now() + Duration::from_secs(5));
                            if !repeated {
                                self.append(
                                    t!("flashing.partitions.failed", error = error),
                                    Tone::Danger,
                                    Instant::now(),
                                );
                            }
                        }
                    }
                }
                Err(TryRecvError::Empty) => self.inspection_receiver = Some(receiver),
                Err(TryRecvError::Disconnected) => {
                    self.inspection = None;
                    self.partition_error = Some(t!("flashing.partitions.interrupted").to_string());
                    self.phase = Phase::Failed;
                    self.source_retry_at = Some(Instant::now() + Duration::from_secs(5));
                    changed = true;
                }
            }
        }
        if let Some(receiver) = self.adb_trigger.take() {
            match receiver.try_recv() {
                Ok(Ok(AdbReboot::Rebooted { serial, output })) => {
                    changed = true;
                    self.append(
                        t!("flashing.log.adb_reboot", serial = serial),
                        Tone::Muted,
                        Instant::now(),
                    );
                    if !output.is_empty() {
                        self.append(output, Tone::Normal, Instant::now());
                    }
                }
                Ok(Ok(AdbReboot::NoDevice)) => {
                    changed = true;
                    self.append(
                        t!("flashing.log.adb_no_device"),
                        Tone::Muted,
                        Instant::now(),
                    );
                }
                Ok(Ok(AdbReboot::MultipleDevices)) => {
                    changed = true;
                    self.append(t!("flashing.adb.multiple"), Tone::Danger, Instant::now());
                }
                Ok(Err(error)) => {
                    changed = true;
                    self.append(
                        t!("flashing.log.adb_failed", error = error),
                        Tone::Danger,
                        Instant::now(),
                    );
                }
                Err(TryRecvError::Empty) => self.adb_trigger = Some(receiver),
                Err(TryRecvError::Disconnected) => changed = true,
            }
            if self.adb_trigger.is_none() {
                self.adb_run = None;
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
        if self.burn_started.is_some() || self.progress.has_active() {
            let second = self.started.elapsed().as_secs();
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
            RunEvent::DevicesPresent { at, devices } => {
                let arrived = devices
                    .iter()
                    .any(|instance| !self.fel_devices.contains(instance));
                self.fel_devices = devices;
                if arrived {
                    self.fel_arrived(at);
                } else if self.fel_devices.is_empty()
                    && matches!(self.phase, Phase::Preparing | Phase::Unclaimed)
                {
                    self.device_lost_at = Some(at);
                }
            }
            RunEvent::DeviceChanged {
                at,
                instance,
                present,
            } => {
                if present {
                    if self.fel_devices.insert(instance) {
                        self.fel_arrived(at);
                    }
                } else if self.fel_devices.remove(&instance)
                    && self.fel_devices.is_empty()
                    && matches!(self.phase, Phase::Preparing | Phase::Unclaimed)
                {
                    self.device_lost_at = Some(at);
                }
            }
            RunEvent::DetectionUnavailable { at, error } => {
                self.append(
                    t!("flashing.log.device_monitor_unavailable", error = error),
                    Tone::Muted,
                    at,
                );
            }
            RunEvent::Line { at, text: line } => {
                let parsed = super::parse_line(&line);
                match &parsed {
                    ParsedLine::Listening => {
                        if !self.phase.is_waiting() || self.listening {
                            return;
                        }
                        self.listening = true;
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
                if self.progress.is_activity(&parsed)
                    && (self.phase.is_waiting()
                        || matches!(self.phase, Phase::Preparing | Phase::Unclaimed))
                {
                    if self.phase.is_waiting() {
                        self.begin_attempt(at);
                    }
                    self.phase = Phase::Burning;
                    self.device_lost_at = None;
                    self.append(t!("flashing.log.device_found"), Tone::Success, at);
                }
                let finished = matches!(
                    parsed,
                    ParsedLine::RunFinished { .. }
                        | ParsedLine::DeviceFinished { .. }
                        | ParsedLine::DeviceMissing { .. }
                );
                self.progress.apply_at(parsed.clone(), at);
                let tone = match &parsed {
                    super::ParsedLine::DeviceFinished { success: true, .. } => Tone::Success,
                    super::ParsedLine::DeviceFinished { success: false, .. } => Tone::Danger,
                    super::ParsedLine::RunFinished { .. } => {
                        if self.progress.has_active() {
                            Tone::Muted
                        } else if self.progress.completion(Some(0), false) == Completion::Success {
                            Tone::Success
                        } else {
                            Tone::Danger
                        }
                    }
                    _ => Tone::Normal,
                };
                self.append(normalize_result_line(&line), tone, at);
                if finished
                    && self.progress.all_finished()
                    && self.last_result.is_none()
                    && self.phase == Phase::Burning
                {
                    self.finish_burn(
                        self.progress.completion(Some(0), false) == Completion::Success,
                        at,
                    );
                }
            }
            RunEvent::Failed { at, error } => {
                self.run = None;
                self.receiver = None;
                self.listening = false;
                self.restart_at = None;
                if self.phase == Phase::Stopped {
                    return;
                }
                let attempted_burn = self.phase == Phase::Burning
                    || (!self.progress.is_empty() && self.last_result.is_none());
                if attempted_burn {
                    self.progress.interrupt_active(at);
                    self.last_result = Some(false);
                    self.phase = Phase::Waiting;
                    self.restart_at = Some(at + Duration::from_millis(400));
                    self.mark_background_activity();
                } else {
                    self.phase = Phase::Failed;
                }
                if let Some(started) = self.burn_started.take() {
                    self.last_duration = Some(at.saturating_duration_since(started));
                }
                self.append(
                    t!("flashing.log.start_failed", error = error),
                    Tone::Danger,
                    at,
                );
            }
            RunEvent::Exited { at, code, stopped } => {
                self.run = None;
                self.receiver = None;
                self.listening = false;
                if stopped || self.phase == Phase::Stopped {
                    self.progress.interrupt_active(at);
                    if self.phase != Phase::Stopped {
                        self.phase = Phase::Stopped;
                    }
                    return;
                }
                let completion = self
                    .progress
                    .completion(code, self.phase.is_waiting() && self.wait_timed_out);
                self.progress.interrupt_active(at);
                if completion == Completion::WaitAgain {
                    self.restart_at = Some(Instant::now() + Duration::from_millis(400));
                    return;
                }
                let success = completion == Completion::Success;
                if success {
                    if self.last_result != Some(true) {
                        self.finish_burn(true, at);
                    }
                    self.restart_at = Some(Instant::now() + Duration::from_millis(400));
                } else {
                    if self.last_result.is_some() {
                        self.phase = Phase::Waiting;
                        self.restart_at = Some(at + Duration::from_millis(400));
                        return;
                    }
                    let attempted_burn = self.phase == Phase::Burning || !self.progress.is_empty();
                    if !attempted_burn {
                        self.burn_started = None;
                        self.burn_log_started = None;
                        self.phase = Phase::Waiting;
                        self.restart_at = Some(at + Duration::from_millis(400));
                        return;
                    }
                    self.last_result = Some(false);
                    self.phase = Phase::Waiting;
                    self.restart_at = Some(at + Duration::from_millis(400));
                    self.mark_background_activity();
                    if let Some(started) = self.burn_started.take() {
                        self.last_duration = Some(at.saturating_duration_since(started));
                    }
                    self.append(
                        t!(
                            "flashing.log.failed",
                            code = code.map_or_else(|| "—".to_string(), |code| code.to_string())
                        ),
                        Tone::Danger,
                        at,
                    );
                }
            }
        }
    }

    fn begin_attempt(&mut self, at: Instant) {
        self.progress.clear();
        self.burn_started = Some(at);
        self.burn_log_started = Some(at.saturating_duration_since(self.started));
        self.last_duration = None;
        self.last_result = None;
        self.shown_second = 0;
        self.listening = false;
        self.wait_timed_out = false;
        self.device_lost_at = None;
        self.mark_background_activity();
    }

    fn fel_arrived(&mut self, at: Instant) {
        self.device_lost_at = None;
        if self.phase.is_waiting() {
            self.begin_attempt(at);
            self.phase = Phase::Preparing;
            self.append(t!("flashing.log.fel_found"), Tone::Success, at);
        }
    }

    fn finish_burn(&mut self, success: bool, at: Instant) {
        self.last_duration = self
            .burn_started
            .take()
            .map(|started| at.saturating_duration_since(started));
        self.last_result = Some(success);
        self.phase = Phase::Waiting;
        self.listening = true;
        self.wait_timed_out = false;
        self.device_lost_at = None;
        self.append(
            if success {
                t!("flashing.log.success")
            } else {
                t!("flashing.result.failed")
            },
            if success { Tone::Success } else { Tone::Danger },
            at,
        );
        self.append(t!("flashing.log.next_device"), Tone::Muted, at);
        self.mark_background_activity();
    }

    fn mark_background_activity(&mut self) {
        if !self.tab_active {
            self.unread_output = true;
        }
    }

    fn log_text(&self) -> String {
        self.logs
            .iter()
            .map(|line| {
                let clock = self.log_clock(line.at);
                format!(
                    "{:02}:{:02} {}\n",
                    clock.as_secs() / 60,
                    clock.as_secs() % 60,
                    line.text
                )
            })
            .collect()
    }

    fn append(&mut self, text: impl ToString, _tone: Tone, now: Instant) {
        let at = now.saturating_duration_since(self.started);
        self.logs.push_back(LogLine {
            at,
            text: text.to_string(),
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

    fn log_clock(&self, at: Duration) -> Duration {
        self.burn_log_started
            .filter(|started| at >= *started)
            .map_or(at, |started| at - started)
    }

    fn phase_label(&self) -> SharedString {
        match self.phase {
            Phase::NoFirmware => t!("flashing.phase.no_firmware"),
            Phase::ReadingFirmware => t!("flashing.phase.reading"),
            Phase::FirmwareMissing => t!("flashing.phase.firmware_missing"),
            Phase::FirmwareUpdating => t!("flashing.phase.firmware_updating"),
            Phase::Ready => t!("flashing.phase.ready"),
            Phase::Waiting => t!("flashing.phase.waiting"),
            Phase::Preparing => t!("flashing.phase.preparing"),
            Phase::Unclaimed => t!("flashing.phase.unclaimed"),
            Phase::Burning => t!("flashing.phase.burning"),
            Phase::Failed => t!("flashing.phase.failed"),
            Phase::Stopped => t!("flashing.phase.stopped"),
        }
    }

    fn phase_hint(&self) -> SharedString {
        match self.phase {
            Phase::NoFirmware => t!("flashing.hint.no_firmware"),
            Phase::ReadingFirmware => t!("flashing.hint.reading"),
            Phase::FirmwareMissing => self
                .source_hint()
                .unwrap_or_else(|| t!("flashing.source.missing")),
            Phase::FirmwareUpdating => t!("flashing.source.updating"),
            Phase::Ready => t!("flashing.partitions.hint"),
            Phase::Waiting if self.last_result == Some(true) => {
                t!("flashing.hint.waiting_after_success")
            }
            Phase::Waiting if self.last_result == Some(false) => {
                t!("flashing.hint.waiting_after_failure")
            }
            Phase::Waiting => t!("flashing.hint.waiting"),
            Phase::Preparing => t!("flashing.hint.preparing"),
            Phase::Unclaimed => t!("flashing.hint.unclaimed"),
            Phase::Burning => t!("flashing.hint.burning"),
            Phase::Failed => t!("flashing.hint.failed"),
            Phase::Stopped => t!("flashing.hint.stopped"),
        }
    }

    fn phase_color(&self, cx: &App) -> Hsla {
        match self.phase {
            Phase::Burning => cx.theme().success,
            Phase::Failed => cx.theme().danger,
            Phase::Unclaimed | Phase::FirmwareMissing | Phase::FirmwareUpdating => {
                cx.theme().warning
            }
            Phase::Waiting | Phase::Preparing => cx.theme().primary,
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
        let adb_ready = self.can_reboot();
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
        let device_count = self.progress.devices().count();
        let progress_label = if device_count == 1 {
            let (device, _) = self.progress.devices().next().unwrap();
            t!("flashing.device.progress", number = device)
        } else {
            t!("flashing.progress")
        };
        let progress_text: SharedString = percent
            .map(|percent| format!("{percent}%").into())
            .unwrap_or_else(|| {
                if self.phase == Phase::Unclaimed {
                    t!("flashing.progress.unclaimed")
                } else if self.is_burning() {
                    t!("flashing.progress.preparing")
                } else {
                    "—".into()
                }
            });
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
                            .id("flashing-phase")
                            .test_support()
                            .aria_label(self.phase_label())
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
                                device_count == 0
                                    && (self.burn_started.is_some()
                                        || self.last_duration.is_some()),
                                |label| {
                                    let text = if matches!(
                                        self.phase,
                                        Phase::Preparing | Phase::Unclaimed
                                    ) {
                                        t!("flashing.detected_elapsed", time = self.elapsed())
                                    } else if self.burn_started.is_some() {
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
            .when(device_count <= 1, |card| {
                card.child(
                    h_flex()
                        .gap_2()
                        .justify_between()
                        .child(
                            div()
                                .id("flashing-progress-label")
                                .test_support()
                                .aria_label(progress_label.clone())
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(progress_label.clone()),
                        )
                        .child(
                            div()
                                .id("flashing-progress-value")
                                .test_support()
                                .aria_label(progress_text.clone())
                                .text_lg()
                                .text_color(color)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(progress_text),
                        ),
                )
                .child(
                    div().id("flashing-primary-progress").test_support().child(
                        Progress::new("flashing-overall")
                            .accessibility_label(progress_label)
                            .rounded_full()
                            .color(color)
                            .loading(self.is_burning() && percent.is_none())
                            .value(percent.map_or(0., f32::from)),
                    ),
                )
                .children(
                    self.progress
                        .devices()
                        .map(|(device, state)| self.device_info(*device, state, cx)),
                )
            })
            .when_some(
                self.progress
                    .stage()
                    .filter(|_| device_count <= 1)
                    .map(str::to_string),
                |card, stage| {
                    card.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("flashing.stage", stage = stage)),
                    )
                },
            )
            .when(device_count > 1, |card| {
                card.child(v_flex().gap_1p5().children(self.progress.devices().map(
                    |(device, state)| {
                        h_flex()
                            .id(("flashing-device-progress", u64::from(*device)))
                            .test_support()
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
                            .child(self.device_info(*device, state, cx))
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
            .when(
                self.last_result.is_some()
                    || self.progress.devices().any(|(_, state)| state.finished),
                |card| card.child(self.result_summary(cx)),
            )
    }

    fn device_info(&self, device: u32, state: &DeviceProgress, cx: &App) -> impl IntoElement {
        let status = if state.interrupted {
            t!("flashing.device.state.interrupted")
        } else if !state.finished {
            t!("flashing.phase.burning")
        } else if state.success {
            t!("flashing.device.state.success")
        } else {
            t!("flashing.device.state.failed")
        };
        let elapsed = state.elapsed(Instant::now()).as_secs();
        let elapsed = t!(
            "flashing.elapsed",
            time = format!("{:02}:{:02}", elapsed / 60, elapsed % 60)
        );
        h_flex()
            .gap_2()
            .text_xs()
            .child(
                div()
                    .id(("flashing-device-state", u64::from(device)))
                    .test_support()
                    .aria_label(status.clone())
                    .text_color(if !state.finished {
                        cx.theme().primary
                    } else if state.success {
                        cx.theme().success
                    } else {
                        cx.theme().danger
                    })
                    .child(status),
            )
            .child(
                div()
                    .id(("flashing-device-elapsed", u64::from(device)))
                    .test_support()
                    .aria_label(elapsed.clone())
                    .text_color(cx.theme().muted_foreground)
                    .child(elapsed),
            )
    }

    fn result_summary(&self, cx: &App) -> impl IntoElement {
        v_flex()
            .gap_1()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .when_some(
                self.last_result.filter(|_| self.progress.is_empty()),
                |summary, success| {
                    let text = if success {
                        t!("flashing.result.success")
                    } else {
                        t!("flashing.result.failed")
                    };
                    summary.child(
                        div()
                            .id("flashing-result-summary")
                            .test_support()
                            .aria_label(text.clone())
                            .text_color(if success {
                                cx.theme().success
                            } else {
                                cx.theme().danger
                            })
                            .child(text),
                    )
                },
            )
            .children(
                self.progress
                    .devices()
                    .filter(|(_, state)| state.finished)
                    .map(|(device, state)| {
                        let device_success = state.success;
                        let text = if state.interrupted {
                            t!("flashing.device.interrupted", number = device)
                        } else if device_success {
                            t!("flashing.device.success", number = device)
                        } else {
                            t!("flashing.device.failed", number = device)
                        };
                        div()
                            .id(("flashing-result-device", u64::from(*device)))
                            .test_support()
                            .aria_label(text.clone())
                            .text_color(if device_success {
                                cx.theme().success
                            } else {
                                cx.theme().danger
                            })
                            .child(text)
                    }),
            )
    }

    fn log_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let dispatch = self.dispatch.clone();
        let log_text = self.log_text();
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
                    .child(
                        div()
                            .id("flashing-log-text")
                            .test_support()
                            .w_full()
                            .text_color(cx.theme().foreground)
                            .child(SelectableText::new("flashing-log-content", log_text)),
                    ),
            )
    }
}

impl EventEmitter<PanelEvent> for FlashingPanel {}
impl EventEmitter<FlashingPanelEvent> for FlashingPanel {}

impl Drop for FlashingPanel {
    fn drop(&mut self) {
        self.shutdown();
    }
}

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
        self.tab_active = active;
        if active {
            self.unread_output = false;
            self.show(window, cx);
        } else if self.is_burning() {
            self.unread_output = true;
            cx.notify();
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
        self.shutdown();
        self._ticks = Task::ready(());
        cx.emit(FlashingPanelEvent::Closed);
    }
}

impl Panel for FlashingPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (group, panel) = (self.tab_group.clone(), cx.entity_id());
        ClosableTabTitle::new("flashing-tab", super::FlashingMark, t!("flashing.title"))
            .unread(self.unread_output)
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
                    .when_some(
                        self.source_hint().filter(|_| self.progress.has_active()),
                        |panel, hint| {
                            panel.child(
                                div()
                                    .id("flashing-source-status")
                                    .test_support()
                                    .aria_label(hint.clone())
                                    .text_sm()
                                    .text_color(cx.theme().warning)
                                    .child(hint),
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

fn normalize_result_line(line: &str) -> String {
    let Some((before, result)) = line.split_once("device burn result=") else {
        return line.to_string();
    };
    let Some((index, digit)) = result
        .char_indices()
        .find(|(_, character)| character.is_ascii_digit())
    else {
        return line.to_string();
    };
    let suffix = &result[index + digit.len_utf8()..];
    if !suffix
        .chars()
        .any(|character| character.is_ascii_alphanumeric())
    {
        return line.to_string();
    }
    format!("{before}device burn result={digit}")
}

#[cfg(test)]
mod tests {
    use gpui_kit::{
        AppContext as _, Context, Entity, IntoElement, Render, TestAppContext, Window,
        component::dock::Panel as _,
    };
    use std::time::{Duration, Instant};

    use super::{FlashingPanel, Phase, RunEvent};

    struct FlashingTitle(Entity<FlashingPanel>);

    impl Render for FlashingTitle {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0
                .update(cx, |panel, cx| panel.title(window, cx).into_any_element())
        }
    }

    fn line_event(text: &str) -> RunEvent {
        RunEvent::Line {
            at: Instant::now(),
            text: text.into(),
        }
    }

    fn exited_event(code: Option<i32>) -> RunEvent {
        RunEvent::Exited {
            at: Instant::now(),
            code,
            stopped: false,
        }
    }

    fn usb_event(at: Instant, present: bool) -> RunEvent {
        RunEvent::DeviceChanged {
            at,
            instance: r"USB\VID_1F3A&PID_EFE8\BOARD".into(),
            present,
        }
    }

    #[test]
    fn corrupted_result_suffixes_are_removed_without_hiding_clean_chinese_text() {
        assert_eq!(
            super::normalize_result_line("line[2199], device burn result=21222222p..."),
            "line[2199], device burn result=2"
        );
        assert_eq!(
            super::normalize_result_line("line[2200], device burn result=1 烧录完成"),
            "line[2200], device burn result=1 烧录完成"
        );
    }

    #[gpui_kit::test]
    fn background_burn_marks_the_flashing_tab_until_it_is_opened(cx: &mut TestAppContext) {
        use gpui_kit::{component::Root, px, size, test::TestWindowExt as _};

        crate::i18n::isolate_thread();
        crate::i18n::set_locale("zh-CN");
        cx.update(crate::init);
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        let (run, _stopped) = super::Run::test_run();
        panel.update(cx, |panel, _| {
            panel.phase = Phase::Waiting;
            panel.run = Some(run);
            panel.tab_active = false;
        });
        let title = cx.new(|_| FlashingTitle(panel.clone()));
        let handle = cx.open_window(size(px(360.), px(80.)), |window, cx| {
            Root::new(title, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window
                    .within("flashing-tab")
                    .try_find("new-output")
                    .is_none()
            );
            panel.update(cx, |panel, cx| {
                panel.handle_event(line_event("device:[2] fes plugin"));
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.within("flashing-tab").find("new-output").visible());
            panel.update(cx, |panel, cx| panel.show(window, cx));
            window.render_frame(cx);
            assert!(
                window
                    .within("flashing-tab")
                    .try_find("new-output")
                    .is_none()
            );
            panel.update(cx, |panel, cx| {
                gpui_kit::base::dock::Panel::set_active(panel, false, window, cx);
                panel.handle_event(line_event("device:[2] burn finish, result=Success"));
                panel.handle_event(line_event("device burn result=2"));
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.within("flashing-tab").find("new-output").visible());
        })
        .unwrap();
        panel.update(cx, |panel, _| {
            assert!(panel.has_unread_output());
        });
    }

    #[gpui_kit::test]
    fn the_flash_log_can_be_selected_and_copied_without_a_copy_button(cx: &mut TestAppContext) {
        use gpui_kit::base::TextSelection;
        use gpui_kit::component::Root;
        use gpui_kit::{point, px, size, test::TestWindowExt as _};

        crate::i18n::isolate_thread();
        crate::i18n::set_locale("zh-CN");
        cx.update(crate::init);
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, _| {
            panel.phase = Phase::Waiting;
            panel.handle_event(line_event("line[2180], device [2] progress = 86%"));
            panel.handle_event(line_event("line[2199], device burn result=2 烧录完成"));
        });
        let handle = cx.open_window(size(px(1000.), px(800.)), |window, cx| {
            Root::new(panel, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("flashing-copy-log").is_none());
            let bounds = window.find("flashing-log-text").bounds();
            window.drag(
                point(bounds.left() + px(2.), bounds.top() + px(3.)),
                point(bounds.left() + px(4.), bounds.bottom() - px(3.)),
                cx,
            );
            window.render_frame(cx);
            let selected = TextSelection::selected_text(window, cx);
            assert!(selected.contains("device [2] progress = 86%"));
            window.press("secondary-c", cx);
        })
        .unwrap();
        let copied = cx.read_from_clipboard().unwrap().text().unwrap();
        assert!(copied.contains("device [2] progress = 86%"));
        assert!(copied.contains("device burn result=2 烧录完成"));
    }

    #[gpui_kit::test]
    fn missing_firmware_is_retained_and_recovers_while_the_tab_is_inactive(
        cx: &mut TestAppContext,
    ) {
        use gpui_kit::component::{Root, dock::BasePanel as _};
        use gpui_kit::test::TestWindowExt as _;
        use gpui_kit::{px, size};

        crate::i18n::isolate_thread();
        crate::i18n::set_locale("zh-CN");
        cx.update(crate::init);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("firmware.img");
        std::fs::write(&path, b"first image").unwrap();
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings.clone(), cx));
        let (run, stopped) = super::Run::test_run();
        let at = Instant::now();
        panel.update(cx, |panel, cx| {
            panel.select_firmware(path.clone(), cx);
            panel.tool_dir = Some(directory.path().to_path_buf());
            panel.update_source(super::source::observe(&path), at);
            panel.firmware_ready(
                super::FirmwareInfo {
                    partitions: vec![super::FirmwarePartition {
                        index: 1,
                        name: "ROOTFS".into(),
                    }],
                    capabilities: super::FirmwareCapabilities::default(),
                },
                cx,
            );
            panel.partition_mode = true;
            panel.selected_partitions.insert(1);
            panel.run = Some(run);
            panel.restart_at = None;
            panel.next_source_probe = at + Duration::from_secs(3600);
        });
        let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
            Root::new(panel.clone(), window, cx)
        });
        std::fs::remove_file(&path).unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            panel.update(cx, |panel, cx| {
                panel.set_active(false, window, cx);
                let (sender, receiver) = std::sync::mpsc::channel();
                sender.send(super::Observation::Missing).unwrap();
                panel.source_probe = Some(receiver);
                panel.tick(cx);
                assert_eq!(panel.phase, Phase::FirmwareMissing);
                assert!(panel.firmware.as_ref() == Some(&path));
                assert!(stopped.load(std::sync::atomic::Ordering::Relaxed));
                let count = panel.logs.len();
                panel.update_source(super::Observation::Missing, at + Duration::from_secs(300));
                panel.tick(cx);
                assert_eq!(panel.logs.len(), count);
            });
            window.render_frame(cx);
            assert_eq!(window.find("flashing-phase").label(), Some("文件不存在"));
            assert_eq!(window.find("flashing-firmware-path").label(), path.to_str());
        })
        .unwrap();
        std::fs::write(&path, b"second image with a new partition layout").unwrap();
        let present = super::source::observe(&path);
        panel.update(cx, |panel, cx| {
            panel.update_source(present.clone(), at + Duration::from_secs(301));
            panel.tick(cx);
            assert_eq!(panel.phase, Phase::FirmwareUpdating);
            panel.update_source(present, at + Duration::from_secs(306));
            panel.apply_source_reload();
            assert_eq!(panel.phase, Phase::ReadingFirmware);
            let (inspection, _) = super::Run::test_run();
            panel.inspection = Some(inspection);
            let (sender, receiver) = std::sync::mpsc::channel();
            sender
                .send(Ok(super::FirmwareInfo {
                    partitions: vec![super::FirmwarePartition {
                        index: 3,
                        name: "ROOTFS".into(),
                    }],
                    capabilities: super::FirmwareCapabilities::default(),
                }))
                .unwrap();
            panel.inspection_receiver = Some(receiver);
            panel.tick(cx);
            assert_eq!(panel.phase, Phase::Waiting);
            assert!(panel.selected_partitions.contains(&3));
            assert!(!panel.selected_partitions.contains(&1));
            assert!(panel.restart_at.is_some());
            assert_eq!(
                settings.read(cx).settings().flashing_firmware_path.as_ref(),
                Some(&path)
            );
        });
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[gpui_kit::test]
    fn source_changes_do_not_kill_active_devices_and_stop_prevents_automatic_recovery(
        cx: &mut TestAppContext,
    ) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        let (run, stopped) = super::Run::test_run();
        panel.update(cx, |panel, cx| {
            panel.phase = Phase::Waiting;
            panel.run = Some(run);
            panel.handle_event(line_event("device:[2] fes plugin"));
            panel.handle_event(line_event("device [2] progress = 30%"));
            panel.update_source(super::Observation::Missing, Instant::now());
            assert!(!panel.apply_source_reload());
            assert_eq!(panel.phase, Phase::Burning);
            assert!(!stopped.load(std::sync::atomic::Ordering::Relaxed));
            assert!(panel.source_hint().is_some());
            panel.handle_event(line_event("device:[2] burn finish, result=Success"));
            panel.apply_source_reload();
            assert_eq!(panel.phase, Phase::FirmwareMissing);
            assert!(stopped.load(std::sync::atomic::Ordering::Relaxed));
            assert!(panel.progress.devices().next().unwrap().1.success);
            panel.stop(cx);
            assert_eq!(panel.phase, Phase::Stopped);
            panel.tick(cx);
            assert!(panel.source_probe.is_none());
            assert!(!panel.apply_source_reload());
            let (sender, receiver) = std::sync::mpsc::channel();
            panel.source_probe = Some(receiver);
            panel.shutdown();
            assert!(sender.send(super::Observation::Missing).is_err());
        });
    }

    #[gpui_kit::test]
    fn rendered_status_changes_on_usb_arrival_before_the_first_percentage(cx: &mut TestAppContext) {
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt as _;
        use gpui_kit::{px, size};

        crate::i18n::isolate_thread();
        crate::i18n::set_locale("zh-CN");
        cx.update(crate::init);
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, _| panel.phase = Phase::Waiting);
        let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
            Root::new(panel.clone(), window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("flashing-phase").label(), Some("等待设备"));
            panel.update(cx, |panel, cx| {
                panel.handle_event(usb_event(Instant::now(), true));
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(window.find("flashing-phase").label(), Some("已检测设备"));
            assert_eq!(
                window.find("flashing-progress-value").label(),
                Some("准备写入…")
            );
            panel.update(cx, |panel, cx| {
                panel.burn_started = Some(Instant::now() - super::DEVICE_CLAIM_NOTICE);
                panel.tick(cx);
            });
            window.render_frame(cx);
            assert_eq!(window.find("flashing-phase").label(), Some("等待工具接管"));
            assert_eq!(
                window.find("flashing-progress-value").label(),
                Some("尚未确认写入")
            );
            panel.update(cx, |panel, cx| {
                assert!(panel.progress.overall_percent().is_none());
                panel.handle_event(line_event("device [3] progress = 1%"));
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(window.find("flashing-phase").label(), Some("正在烧录"));
            assert_eq!(window.find("flashing-progress-value").label(), Some("1%"));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn eight_device_ui_keeps_other_writes_running_after_success_failure_and_slot_reuse(
        cx: &mut TestAppContext,
    ) {
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt as _;
        use gpui_kit::{px, size};

        crate::i18n::isolate_thread();
        crate::i18n::set_locale("zh-CN");
        cx.update(crate::init);
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        let start = Instant::now() - Duration::from_secs(20);
        let (run, stopped) = super::Run::test_run();
        panel.update(cx, |panel, _| {
            panel.phase = Phase::Waiting;
            panel.run = Some(run);
            for device in 1..=8 {
                panel.handle_event(RunEvent::Line {
                    at: start + Duration::from_secs(device as u64),
                    text: format!("device:[{device}] fes plugin"),
                });
                panel.handle_event(RunEvent::Line {
                    at: start + Duration::from_secs(9),
                    text: format!("device [{device}] progress = {}%", device * 5),
                });
            }
            assert_eq!(panel.progress.devices().count(), 8);
        });
        let handle = cx.open_window(size(px(1400.), px(900.)), |window, cx| {
            Root::new(panel.clone(), window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("flashing-primary-progress").is_none());
            for device in 1..=8_u64 {
                assert!(window.find(("flashing-device-progress", device)).visible());
                assert_eq!(
                    window.find(("flashing-device-state", device)).label(),
                    Some("正在烧录")
                );
                assert!(window.find(("flashing-device-elapsed", device)).visible());
            }
            panel.update(cx, |panel, cx| {
                panel.handle_event(RunEvent::Line {
                    at: start + Duration::from_secs(10),
                    text: "device:[1] burn finish, result=Success".into(),
                });
                panel.handle_event(line_event("device burn result=2"));
                panel.handle_event(RunEvent::Line {
                    at: start + Duration::from_secs(12),
                    text: "device:[2] burn finish, result=Failed".into(),
                });
                panel.handle_event(line_event("device [3] progress = 62%"));
                assert_eq!(panel.phase, Phase::Burning);
                assert!(panel.last_result.is_none());
                assert!(!panel.can_configure());
                assert!(panel.restart_at.is_none());
                assert!(!stopped.load(std::sync::atomic::Ordering::Relaxed));
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(
                window.find(("flashing-device-elapsed", 1_u64)).label(),
                Some("已用时 00:09")
            );
            assert_eq!(
                window.find(("flashing-device-elapsed", 2_u64)).label(),
                Some("已用时 00:10")
            );
            assert_eq!(
                window.find(("flashing-result-device", 1_u64)).label(),
                Some("设备 1 烧录成功")
            );
            assert_eq!(
                window.find(("flashing-result-device", 2_u64)).label(),
                Some("设备 2 烧录失败")
            );
            assert_eq!(
                window.find(("flashing-device-state", 8_u64)).label(),
                Some("正在烧录")
            );
            panel.update(cx, |panel, cx| {
                let other = panel
                    .progress
                    .devices()
                    .find(|(device, _)| **device == 8)
                    .unwrap()
                    .1
                    .clone();
                panel.handle_event(line_event("device:[1] fes plugin"));
                panel.handle_event(line_event("device [1] progress = 1%"));
                assert_eq!(
                    panel
                        .progress
                        .devices()
                        .find(|(device, _)| **device == 8)
                        .unwrap()
                        .1,
                    &other
                );
                assert_eq!(panel.phase, Phase::Burning);
                assert!(panel.restart_at.is_none());
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.try_find(("flashing-result-device", 1_u64)).is_none());
            assert_eq!(
                window.find(("flashing-device-state", 1_u64)).label(),
                Some("正在烧录")
            );
            assert_eq!(
                window.find(("flashing-device-elapsed", 2_u64)).label(),
                Some("已用时 00:10")
            );
            panel.update(cx, |panel, cx| {
                panel.handle_event(exited_event(Some(-1073741819)));
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(
                window.find(("flashing-result-device", 2_u64)).label(),
                Some("设备 2 烧录失败")
            );
            assert_eq!(
                window.find(("flashing-device-state", 8_u64)).label(),
                Some("已中断")
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn progress_bars_follow_actual_device_count_and_results_name_each_device(
        cx: &mut TestAppContext,
    ) {
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt as _;
        use gpui_kit::{px, size};

        crate::i18n::isolate_thread();
        crate::i18n::set_locale("zh-CN");
        cx.update(crate::init);
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, _| panel.phase = Phase::Waiting);
        let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
            Root::new(panel.clone(), window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("flashing-progress-label").label(),
                Some("烧录进度")
            );
            panel.update(cx, |panel, cx| {
                panel.handle_event(line_event("device [2] progress = 40%"));
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.find("flashing-primary-progress").visible());
            assert_eq!(
                window.find("flashing-progress-label").label(),
                Some("设备 2 烧录进度")
            );
            assert_eq!(window.find("flashing-progress-value").label(), Some("40%"));
            assert!(
                window
                    .try_find(("flashing-device-progress", 2_u64))
                    .is_none()
            );
            panel.update(cx, |panel, cx| {
                panel.handle_event(line_event("device:[2] burn finish, result=Success"));
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(
                window.find(("flashing-result-device", 2_u64)).label(),
                Some("设备 2 烧录成功")
            );
            assert!(window.try_find("flashing-result-summary").is_none());
            panel.update(cx, |panel, cx| {
                panel.handle_event(line_event("device:[2] fes plugin"));
                panel.handle_event(line_event("device [3] progress = 20%"));
                panel.handle_event(line_event("device:[2] burn finish, result=Success"));
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.try_find("flashing-primary-progress").is_none());
            assert!(window.try_find("flashing-progress-label").is_none());
            assert!(window.find(("flashing-device-progress", 2_u64)).visible());
            assert!(window.find(("flashing-device-progress", 3_u64)).visible());
            assert!(window.try_find(("flashing-result-device", 3_u64)).is_none());
            panel.update(cx, |panel, cx| {
                panel.handle_event(line_event("device:[3] burn finish, result=Failed"));
                panel.handle_event(line_event("device burn result=2"));
                cx.notify();
            });
            window.render_frame(cx);
            assert_eq!(
                window.find(("flashing-result-device", 2_u64)).label(),
                Some("设备 2 烧录成功")
            );
            assert_eq!(
                window.find(("flashing-result-device", 3_u64)).label(),
                Some("设备 3 烧录失败")
            );
            panel.update(cx, |panel, cx| {
                panel.handle_event(line_event("device:[5] fes plugin"));
                panel.handle_event(line_event("device [5] progress = 1%"));
                cx.notify();
            });
            window.render_frame(cx);
            assert!(window.find("flashing-primary-progress").visible());
            assert_eq!(
                window.find("flashing-progress-label").label(),
                Some("设备 5 烧录进度")
            );
            for device in [2_u64, 3_u64, 5_u64] {
                assert!(
                    window
                        .try_find(("flashing-device-progress", device))
                        .is_none()
                );
                assert!(
                    window
                        .try_find(("flashing-result-device", device))
                        .is_none()
                );
            }
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn a_detected_device_without_tool_confirmation_shows_recovery_without_restarting(
        cx: &mut TestAppContext,
    ) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, cx| {
            panel.phase = Phase::Waiting;
            let (run, stopped) = super::Run::test_run();
            panel.run = Some(run);
            let detected = Instant::now() - super::DEVICE_CLAIM_NOTICE - Duration::from_secs(1);
            panel.handle_event(usb_event(detected, true));
            panel.tick(cx);
            assert_eq!(panel.phase, Phase::Unclaimed);
            assert!(panel.can_configure());
            assert!(panel.running());
            assert!(!panel.is_burning());
            assert!(panel.last_result.is_none());
            assert!(panel.restart_at.is_none());
            assert!(!stopped.load(std::sync::atomic::Ordering::Relaxed));
            let count = panel.logs.len();
            panel.tick(cx);
            panel.handle_event(usb_event(Instant::now(), true));
            assert_eq!(panel.logs.len(), count);
            assert_eq!(panel.phase, Phase::Unclaimed);
            panel.handle_event(line_event("device [3] progress = 1%"));
            assert_eq!(panel.phase, Phase::Burning);
            assert_eq!(panel.burn_started, Some(detected));
            panel.tick(cx);
            assert_eq!(panel.phase, Phase::Burning);
        });
    }

    #[gpui_kit::test]
    fn preparation_exit_is_not_a_failed_burn_and_start_errors_do_not_override_last_success(
        cx: &mut TestAppContext,
    ) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, cx| {
            panel.phase = Phase::Waiting;
            panel.handle_event(usb_event(Instant::now() - Duration::from_secs(30), true));
            panel.tick(cx);
            assert_eq!(panel.phase, Phase::Unclaimed);
            panel.handle_event(line_event("device burn result=2"));
            assert_eq!(panel.phase, Phase::Unclaimed);
            assert!(panel.last_result.is_none());
            panel.handle_event(exited_event(Some(2)));
            assert_eq!(panel.phase, Phase::Waiting);
            assert!(panel.last_result.is_none());
            assert!(panel.burn_started.is_none());
            panel.last_result = Some(true);
            panel.progress.apply(super::ParsedLine::DeviceFinished {
                device: 3,
                success: true,
            });
            panel.handle_event(RunEvent::Failed {
                at: Instant::now(),
                error: "PhoenixSuit.exe is running".into(),
            });
            assert_eq!(panel.phase, Phase::Failed);
            assert_eq!(panel.last_result, Some(true));
            assert!(panel.restart_at.is_none());
        });
    }

    #[gpui_kit::test]
    fn fel_arrival_shows_activity_before_any_console_output_and_preserves_the_clock(
        cx: &mut TestAppContext,
    ) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, _| {
            panel.phase = Phase::Waiting;
            panel.last_result = Some(false);
            panel.progress.apply(super::ParsedLine::Progress {
                device: 3,
                percent: 83,
            });
            let detected_at = Instant::now() - Duration::from_secs(8);
            panel.handle_event(usb_event(detected_at, true));
            assert_eq!(panel.phase, Phase::Preparing);
            assert!(panel.is_burning());
            assert!(!panel.can_configure());
            assert_eq!(panel.burn_started, Some(detected_at));
            assert!(panel.last_result.is_none());
            assert!(panel.progress.overall_percent().is_none());
            assert!(panel.phase_hint().contains("FEL"));
            let count = panel.logs.len();
            panel.handle_event(usb_event(Instant::now(), true));
            assert_eq!(panel.logs.len(), count);
            panel.handle_event(usb_event(Instant::now(), false));
            panel.handle_event(usb_event(Instant::now(), true));
            assert_eq!(panel.burn_started, Some(detected_at));
            panel.handle_event(line_event("device:[3] fes plugin"));
            assert_eq!(panel.phase, Phase::Burning);
            assert_eq!(panel.burn_started, Some(detected_at));
            panel.handle_event(line_event("device [3] progress = 12%"));
            assert_eq!(panel.progress.overall_percent(), Some(12));
            panel.handle_event(line_event("device:[3] burn finish, result=Success"));
            panel.handle_event(line_event("device burn result=2"));
            assert_eq!(panel.last_result, Some(true));
            assert_eq!(panel.elapsed(), "00:08");
            let devices = panel.fel_devices.clone();
            panel.handle_event(RunEvent::DevicesPresent {
                at: Instant::now(),
                devices,
            });
            assert_eq!(panel.phase, Phase::Waiting);
            assert_eq!(panel.last_result, Some(true));
            panel.handle_event(usb_event(Instant::now(), false));
            panel.handle_event(usb_event(Instant::now(), true));
            assert_eq!(panel.phase, Phase::Preparing);
            assert!(panel.last_result.is_none());
            assert!(panel.progress.is_empty());
        });
    }

    #[gpui_kit::test]
    fn unplugging_before_writes_restores_waiting_but_usb_handover_does_not_interrupt_writes(
        cx: &mut TestAppContext,
    ) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, cx| {
            panel.phase = Phase::Waiting;
            panel.handle_event(usb_event(Instant::now(), true));
            panel.handle_event(usb_event(Instant::now() - Duration::from_secs(3), false));
            panel.tick(cx);
            assert_eq!(panel.phase, Phase::Waiting);
            assert!(panel.can_configure());
            assert!(panel.last_result.is_none());
            assert!(panel.burn_started.is_none());
            panel.handle_event(usb_event(Instant::now(), true));
            panel.handle_event(line_event("device:[3] fes plugin"));
            panel.handle_event(usb_event(Instant::now() - Duration::from_secs(3), false));
            panel.tick(cx);
            assert_eq!(panel.phase, Phase::Burning);
            panel.stop(cx);
            panel.handle_event(usb_event(Instant::now(), true));
            assert_eq!(panel.phase, Phase::Stopped);
        });
    }

    #[gpui_kit::test]
    fn closing_cancels_every_owned_run_and_discards_pending_restart_events(
        cx: &mut TestAppContext,
    ) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, cx| {
            let (run, run_stopped) = super::Run::test_run();
            let (inspection, inspection_stopped) = super::Run::test_run();
            let (retiring, retiring_stopped) = super::Run::test_run();
            let (adb, adb_stopped) = super::Run::test_run();
            panel.run = Some(run);
            panel.inspection = Some(inspection);
            panel.retiring = Some(retiring);
            panel.adb_run = Some(adb);
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            sender.send(line_event("device:[1] fes plugin")).unwrap();
            panel.receiver = Some(receiver);
            let (inspection_sender, receiver) = std::sync::mpsc::channel();
            panel.inspection_receiver = Some(receiver);
            let (adb_sender, receiver) = std::sync::mpsc::channel();
            panel.adb_trigger = Some(receiver);
            panel.phase = Phase::Waiting;
            panel.picking = true;
            panel.listening = true;
            panel.restart_at = Some(Instant::now());

            panel.shutdown();
            for stopped in [
                run_stopped,
                inspection_stopped,
                retiring_stopped,
                adb_stopped,
            ] {
                assert!(stopped.load(std::sync::atomic::Ordering::Relaxed));
            }
            assert!(sender.send(line_event("device burn result=1")).is_err());
            assert!(inspection_sender.send(Err("late result".into())).is_err());
            assert!(adb_sender.send(Err("late ADB result".into())).is_err());
            assert!(!panel.picking);
            assert!(!panel.listening);
            assert!(panel.restart_at.is_none());
            assert!(panel.run.is_none());
            assert!(panel.inspection.is_none());
            assert!(panel.retiring.is_none());
            assert!(panel.adb_run.is_none());
            panel.command(
                super::FlashingCommand::SetMode(super::EraseMode::EraseAll),
                cx,
            );
            panel.tick(cx);
            assert_eq!(panel.phase, Phase::Stopped);
            assert!(panel.restart_at.is_none());
        });
    }

    #[gpui_kit::test]
    fn releasing_the_panel_cancels_its_background_run(cx: &mut TestAppContext) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        let (run, stopped) = super::Run::test_run();
        panel.update(cx, |panel, _| panel.run = Some(run));
        cx.update(|_| drop(panel));
        cx.run_until_parked();
        assert!(stopped.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[gpui_kit::test]
    fn completed_and_failed_burns_both_return_to_waiting(cx: &mut TestAppContext) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, _| {
            panel.phase = Phase::Waiting;
            panel.handle_event(line_event("line[42], device:[1] fes plugin"));
            assert_eq!(panel.phase, Phase::Burning);
            panel.handle_event(line_event("line[43], device [1] progress = 45%"));
            assert_eq!(panel.progress.overall_percent(), Some(45));
            panel.handle_event(line_event("device:[1] burn finish, result=Success"));
            panel.handle_event(line_event("line[44], device burn result=1"));
            panel.handle_event(exited_event(Some(0)));
            assert_eq!(panel.phase, Phase::Waiting);
            assert_eq!(panel.last_result, Some(true));
            assert!(panel.restart_at.take().is_some());
            assert_eq!(panel.progress.overall_percent(), Some(100));

            panel.handle_event(line_event("line[42], device:[1] fes plugin"));
            panel.handle_event(line_event("line[44], device burn result=0"));
            panel.handle_event(exited_event(Some(0)));
            assert_eq!(panel.phase, Phase::Waiting);
            assert_eq!(panel.last_result, Some(false));
            assert!(panel.restart_at.take().is_some());

            panel.phase = Phase::Stopped;
            panel.handle_event(line_event("line[44], device burn result=1"));
            panel.handle_event(exited_event(Some(0)));
            assert_eq!(panel.phase, Phase::Stopped);
            assert!(panel.restart_at.is_none());
        });
    }

    #[gpui_kit::test]
    fn confirmed_device_success_survives_unused_slots_and_console_timeout(cx: &mut TestAppContext) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, _| {
            panel.phase = Phase::Waiting;
            for line in [
                "line[2141], device:[3] fes plugin",
                "line[2153], device [3] progress = 83%",
                "line[2172], device:[3] burn finish, result=Success",
                "line[2192], device:[1]: None, device not plug in, result=No device",
                "line[2199], device burn result=2",
                "line[2235], Time out, result = 2",
            ] {
                panel.handle_event(line_event(line));
            }
            assert_eq!(panel.phase, Phase::Waiting);
            assert_eq!(panel.last_result, Some(true));
            assert_eq!(panel.progress.overall_percent(), Some(100));
            panel.handle_event(exited_event(Some(2)));
            assert_eq!(panel.last_result, Some(true));
            assert!(panel.restart_at.take().is_some());
            panel.handle_event(line_event("Detected 1 device, flashing..."));
            assert_eq!(panel.phase, Phase::Burning);
            assert!(panel.progress.is_empty());
            assert!(panel.last_result.is_none());
            assert!(panel.burn_started.is_some());
        });
    }

    #[gpui_kit::test]
    fn a_crashed_burn_keeps_its_failure_until_the_next_device_is_detected(cx: &mut TestAppContext) {
        let settings = cx.new(|_| crate::settings::SettingsStore::in_memory());
        let panel = cx.new(|cx| FlashingPanel::new(cx.focus_handle(), settings, cx));
        panel.update(cx, |panel, _| {
            panel.phase = Phase::Waiting;
            panel.handle_event(line_event("device:[1] fes plugin"));
            panel.handle_event(line_event("device [1] progress = 68%"));
            panel.handle_event(exited_event(Some(-1073741819)));
            assert_eq!(panel.phase, Phase::Waiting);
            assert_eq!(panel.last_result, Some(false));
            assert_eq!(panel.progress.overall_percent(), Some(68));
            assert!(panel.restart_at.take().is_some());
            assert!(!panel.listening);

            panel.handle_event(line_event("device:[1] fes plugin"));
            assert_eq!(panel.phase, Phase::Burning);
            assert_eq!(panel.progress.overall_percent(), Some(0));
            assert!(panel.last_result.is_none());
            panel.handle_event(line_event("device [1] progress = 1%"));
            assert_eq!(panel.progress.overall_percent(), Some(1));
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
                panel.handle_event(line_event(
                    "line[1591], usbproproc service is ready, listening device...",
                ));
            }
            assert_eq!(panel.logs.len(), 1);
            assert!(panel.burn_started.is_none());
            panel.handle_event(line_event(
                "line[50], error: no device checkIn in 30 senconds, will end this process!",
            ));
            assert_eq!(panel.logs.len(), 1);
            panel.handle_event(exited_event(Some(1)));
            assert_eq!(panel.phase, Phase::Waiting);
            assert!(panel.restart_at.is_some());
            assert!(panel.last_result.is_none());

            panel.restart_at = None;
            panel.started = Instant::now() - Duration::from_secs(20);
            let detected_at = Instant::now() - Duration::from_secs(8);
            panel.handle_event(RunEvent::Line {
                at: detected_at,
                text: "device:[1] fes plugin".into(),
            });
            assert_eq!(panel.phase, Phase::Burning);
            assert!(panel.burn_started.is_some());
            assert_eq!(
                panel.log_clock(panel.logs.back().unwrap().at),
                Duration::ZERO
            );
            panel.handle_event(line_event("device [1] progress = 100%"));
            panel.handle_event(line_event("device:[1] burn finish, result=Success"));
            panel.handle_event(line_event("device burn result=1"));
            assert_eq!(panel.phase, Phase::Waiting);
            assert!(panel.burn_started.is_none());
            assert_eq!(panel.elapsed(), "00:08");
            assert!(panel.log_clock(panel.logs.back().unwrap().at) >= Duration::from_secs(8));
            assert_eq!(panel.last_result, Some(true));
            assert!(panel.restart_at.is_none());
            panel.handle_event(line_event("device:[1] fes plugin"));
            assert_eq!(panel.phase, Phase::Burning);
            assert!(panel.burn_started.is_some());
            assert_eq!(panel.progress.overall_percent(), Some(0));
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
                        at: Instant::now(),
                        code: Some(1),
                        stopped: true
                    })
                    .is_err()
            );
            assert_eq!(panel.phase, Phase::Waiting);
            assert!(panel.restart_at.is_some());

            let (sender, receiver) = std::sync::mpsc::channel();
            panel.receiver = Some(receiver);
            sender.send(line_event("device:[1] fes plugin")).unwrap();
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
            assert!(sender.send(line_event("device:[1] fes plugin")).is_err());
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
