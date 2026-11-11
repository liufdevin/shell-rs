use regex::{Regex, RegexBuilder};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use super::{
    commands::{self, Privilege, Source},
    model::{self, LogBuffer, LogLevel, LogLine},
    options::{self, LogOptions},
    report::{self, ReportFile, ReportOptions, ReportResult},
};
use crate::{
    app::{CatalogIcon, DeviceLogAction},
    host::{DeviceLogSettings, HostId, HostStore, MonitorSettings},
    i18n::{UiLocale, t},
    monitor::{MonitorConnector, MonitorLink},
    settings::{DEFAULT_INTERFACE_FONT_SIZE, SettingsStore},
    terminal::{ExecTarget, RemoteTerminalId, TerminalLifecycle, exec_answer},
};
use gpui_kit::{
    component::{
        ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _,
        button::{Button, ButtonVariants as _},
        checkbox::Checkbox,
        h_flex,
        input::{Input, InputEvent, InputState},
        scroll::ScrollableElement as _,
        searchable_list::SearchableListItem,
        select::{Select, SelectEvent, SelectState},
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder as _,
    *,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceLogCommand {
    ToggleSplit,
    FontSize(u8),
    Search,
    SearchNext,
    SearchPrevious,
    ClearSearch,
    ToggleSearchRegex,
    ToggleSearchCaseSensitive,
    ToggleMatchingLines,
    Pause,
    Clear,
    Export,
    Retry,
    CurrentUser,
    Level(LogLevel),
    Follow(bool),
    Filters,
    ApplyOptions(LogOptions),
    OpenService(String),
    Bookmark(u64),
    Bookmarks,
    Copy(u64),
    Diagnostics,
    CollectReport(ReportOptions),
    CancelReport,
}

#[derive(Clone, PartialEq)]
pub struct LogTarget {
    exec: ExecTarget,
    host: HostId,
    settings: DeviceLogSettings,
    serial: Option<MonitorSettings>,
}

impl LogTarget {
    pub fn new(
        exec: ExecTarget,
        host: HostId,
        settings: DeviceLogSettings,
        serial: Option<MonitorSettings>,
    ) -> Self {
        Self {
            exec,
            host,
            settings,
            serial,
        }
    }
}

#[derive(Clone)]
struct LevelChoice(LogLevel);
impl SearchableListItem for LevelChoice {
    type Value = LogLevel;
    fn title(&self) -> SharedString {
        level_label(self.0)
    }
    fn value(&self) -> &LogLevel {
        &self.0
    }
}

fn level_label(level: LogLevel) -> SharedString {
    match level {
        LogLevel::All => t!("device_logs.level.all"),
        LogLevel::Verbose => "Verbose".into(),
        LogLevel::Debug => "Debug".into(),
        LogLevel::Info => "Info".into(),
        LogLevel::Warning => "Warning".into(),
        LogLevel::Error => "Error".into(),
        LogLevel::Fatal => "Fatal".into(),
    }
}

#[derive(Clone)]
enum Problem {
    NotConnected,
    Busy,
    NoChannel,
    Unsupported,
    Elevation,
    Read(String),
}

#[derive(Default)]
struct LogState {
    settings: DeviceLogSettings,
    generation: Option<u64>,
    buffer: LogBuffer,
    paused: Option<Vec<LogLine>>,
    problem: Option<Problem>,
    source: Option<Source>,
    route: String,
    elevated: bool,
    current_user: bool,
    clear_epoch: u64,
    options: LogOptions,
    bookmarks: HashSet<u64>,
    app_waiting: bool,
}

struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Default)]
struct SearchState {
    query: String,
    regex: bool,
    case_sensitive: bool,
    matching_lines: bool,
    compiled: Option<Rc<Regex>>,
    error: Option<String>,
    matches: Rc<Vec<u64>>,
    selected: Option<u64>,
}

impl SearchState {
    fn has_query(&self) -> bool {
        !self.query.trim().is_empty()
    }

    fn compile(&mut self) -> Option<Rc<Regex>> {
        self.error = None;
        self.compiled = None;
        if !self.has_query() {
            return None;
        }
        let pattern = if self.regex {
            self.query.trim().to_string()
        } else {
            regex::escape(self.query.trim())
        };
        match RegexBuilder::new(&pattern)
            .case_insensitive(!self.case_sensitive)
            .size_limit(2 * 1024 * 1024)
            .build()
        {
            Ok(regex) => {
                let regex = Rc::new(regex);
                self.compiled = Some(regex.clone());
                Some(regex)
            }
            Err(error) => {
                self.error = Some(error.to_string());
                None
            }
        }
    }
}

fn spans(regex: &Regex, text: &str) -> Vec<Range<usize>> {
    regex
        .find_iter(text)
        .filter(|found| !found.is_empty())
        .map(|found| found.range())
        .collect()
}

fn has_match(regex: &Regex, text: &str) -> bool {
    regex.find_iter(text).any(|found| !found.is_empty())
}

pub struct DeviceLogPanel {
    settings: Entity<SettingsStore>,
    split: bool,
    dispatch: FocusHandle,
    store: Entity<HostStore>,
    connector: Option<Arc<dyn MonitorConnector>>,
    target: Option<LogTarget>,
    active: bool,
    states: HashMap<RemoteTerminalId, LogState>,
    keyword: Entity<InputState>,
    level: Entity<SelectState<Vec<LevelChoice>>>,
    minimum: LogLevel,
    search: SearchState,
    visible: Rc<Vec<LogLine>>,
    follow: bool,
    scroll: UniformListScrollHandle,
    capture: Option<Task<()>>,
    status: Option<SharedString>,
    bookmarks_only: bool,
    report_task: Option<Task<()>>,
    report_progress: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl DeviceLogPanel {
    pub fn new(
        dispatch: FocusHandle,
        store: Entity<HostStore>,
        settings: Entity<SettingsStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let keyword =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("device_logs.keyword")));
        let level = cx.new(|cx| {
            SelectState::new(
                LogLevel::ALL
                    .into_iter()
                    .map(LevelChoice)
                    .collect::<Vec<_>>(),
                None,
                window,
                cx,
            )
        });
        let filter_dispatch = dispatch.clone();
        let level_dispatch = dispatch.clone();
        let subscriptions = vec![
            cx.observe(&settings, |_, _, cx| cx.notify()),
            cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
                this.keyword.update(cx, |input, cx| {
                    input.set_placeholder(t!("device_logs.keyword"), window, cx)
                });
                cx.notify();
            }),
            cx.subscribe_in(
                &keyword,
                window,
                move |_, _, event: &InputEvent, window, cx| {
                    let command = match event {
                        InputEvent::Change => DeviceLogCommand::Search,
                        InputEvent::PressEnter { shift: false, .. } => DeviceLogCommand::SearchNext,
                        InputEvent::PressEnter { shift: true, .. } => {
                            DeviceLogCommand::SearchPrevious
                        }
                        _ => return,
                    };
                    let dispatch = filter_dispatch.clone();
                    window.defer(cx, move |window, cx| {
                        dispatch.dispatch_action(&DeviceLogAction(command), window, cx)
                    });
                },
            ),
            cx.subscribe_in(&level, window, move |_, _, event, window, cx| {
                if let SelectEvent::Confirm(Some(level)) = event {
                    let dispatch = level_dispatch.clone();
                    let level = *level;
                    window.defer(cx, move |window, cx| {
                        dispatch.dispatch_action(
                            &DeviceLogAction(DeviceLogCommand::Level(level)),
                            window,
                            cx,
                        )
                    });
                }
            }),
        ];
        Self {
            settings,
            split: false,
            dispatch,
            store,
            connector: None,
            target: None,
            active: false,
            states: HashMap::new(),
            keyword,
            level,
            minimum: LogLevel::All,
            search: SearchState::default(),
            visible: Rc::new(Vec::new()),
            follow: true,
            scroll: UniformListScrollHandle::new(),
            capture: None,
            status: None,
            bookmarks_only: false,
            report_task: None,
            report_progress: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_connector(&mut self, connector: Arc<dyn MonitorConnector>, cx: &mut Context<Self>) {
        self.connector = Some(connector);
        self.restart(cx);
    }

    pub(crate) fn set_split(&mut self, split: bool, cx: &mut Context<Self>) {
        if self.split != split {
            self.split = split;
            cx.notify();
        }
    }

    pub fn set_target(&mut self, target: Option<LogTarget>, active: bool, cx: &mut Context<Self>) {
        if self.target == target && self.active == active {
            return;
        }
        if let Some(target) = &target
            && self
                .states
                .get(&target.exec.terminal)
                .is_some_and(|state| state.settings != target.settings)
        {
            self.states.remove(&target.exec.terminal);
        }
        self.target = target;
        self.active = active;
        self.status = None;
        self.report_task = None;
        self.report_progress = None;
        self.restart(cx);
        self.refresh_visible();
        self.follow_latest();
        cx.notify();
    }

    pub fn forget(&mut self, terminal: RemoteTerminalId, cx: &mut Context<Self>) {
        self.states.remove(&terminal);
        if self
            .target
            .as_ref()
            .is_some_and(|target| target.exec.terminal == terminal)
        {
            self.capture = None;
            self.report_task = None;
            self.report_progress = None;
            self.target = None;
        }
        self.refresh_visible();
        cx.notify();
    }

    fn state(&self) -> Option<&LogState> {
        self.target
            .as_ref()
            .and_then(|target| self.states.get(&target.exec.terminal))
    }

    fn base_lines(&self) -> Vec<LogLine> {
        let Some(state) = self.state() else {
            return Vec::new();
        };
        state
            .paused
            .clone()
            .unwrap_or_else(|| state.buffer.lines())
            .into_iter()
            .filter(|line| model::matches(line, self.minimum, ""))
            .filter(|line| !self.bookmarks_only || state.bookmarks.contains(&line.id))
            .filter(|line| {
                state.source != Some(Source::Android)
                    || model::matches_options(line, &state.options, state.buffer.clock())
            })
            .collect()
    }

    fn refresh_search(&mut self) {
        let lines = self.base_lines();
        let regex = self.search.compile();
        self.search.matches = Rc::new(
            regex
                .as_ref()
                .map(|regex| {
                    lines
                        .iter()
                        .filter(|line| has_match(regex, &line.text))
                        .map(|line| line.id)
                        .collect()
                })
                .unwrap_or_default(),
        );
        if self
            .search
            .selected
            .is_some_and(|id| !self.search.matches.contains(&id))
        {
            self.search.selected = None;
        }
        self.visible = Rc::new(
            if self.search.matching_lines && self.search.has_query() && regex.is_some() {
                lines
                    .into_iter()
                    .filter(|line| self.search.matches.contains(&line.id))
                    .collect()
            } else {
                lines
            },
        );
    }

    fn select_search_match(&mut self, index: usize) {
        self.search.selected = self.search.matches.get(index).copied();
        self.scroll_to_selected();
    }

    fn step_search(&mut self, forward: bool) {
        let total = self.search.matches.len();
        if total == 0 {
            self.search.selected = None;
            return;
        }
        let current = self.search.selected.and_then(|id| {
            self.search
                .matches
                .iter()
                .position(|candidate| *candidate == id)
        });
        let index = match (current, forward) {
            (Some(current), true) => (current + 1) % total,
            (Some(current), false) => (current + total - 1) % total,
            (None, true) => 0,
            (None, false) => total - 1,
        };
        self.select_search_match(index);
    }

    fn scroll_to_selected(&self) {
        if let Some(id) = self.search.selected
            && let Some(index) = self.visible.iter().position(|line| line.id == id)
        {
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.keyword
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.search.query.clear();
        self.search.selected = None;
        self.search.compiled = None;
        self.search.error = None;
        self.search.matches = Rc::default();
        self.refresh_search();
        self.follow_latest();
        self.keyword.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn handle_search_command(
        &mut self,
        command: &DeviceLogCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match command {
            DeviceLogCommand::Search => {
                let query = self.keyword.read(cx).value().to_string();
                if self.search.query == query {
                    return true;
                }
                self.search.query = query;
                self.refresh_search();
                self.search.selected = self.search.matches.first().copied();
                if self.search.has_query() {
                    self.scroll_to_selected();
                } else {
                    self.follow_latest();
                }
            }
            DeviceLogCommand::SearchNext => self.step_search(true),
            DeviceLogCommand::SearchPrevious => self.step_search(false),
            DeviceLogCommand::ClearSearch => {
                self.clear_search(window, cx);
                return true;
            }
            DeviceLogCommand::ToggleSearchRegex => {
                self.search.regex = !self.search.regex;
                self.refresh_search();
                self.search.selected = self.search.matches.first().copied();
                self.scroll_to_selected();
            }
            DeviceLogCommand::ToggleSearchCaseSensitive => {
                self.search.case_sensitive = !self.search.case_sensitive;
                self.refresh_search();
                self.search.selected = self.search.matches.first().copied();
                self.scroll_to_selected();
            }
            DeviceLogCommand::ToggleMatchingLines => {
                self.search.matching_lines = !self.search.matching_lines;
                self.refresh_search();
                self.scroll_to_selected();
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    pub fn command(
        &mut self,
        command: &DeviceLogCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.active {
            return;
        }
        let Some(id) = self.target.as_ref().map(|target| target.exec.terminal) else {
            return;
        };
        self.status = None;
        if self.handle_search_command(command, window, cx) {
            return;
        }
        match command {
            DeviceLogCommand::ToggleSplit => return,
            DeviceLogCommand::FontSize(size) => self.settings.update(cx, |store, cx| {
                store.update(
                    |settings| settings.device_logs.font_size = (*size).clamp(12, 24),
                    cx,
                )
            }),
            DeviceLogCommand::Pause => {
                let state = self.states.entry(id).or_default();
                state.paused = if state.paused.is_some() {
                    None
                } else {
                    Some(state.buffer.lines())
                };
            }
            DeviceLogCommand::Clear => {
                let state = self.states.entry(id).or_default();
                state.buffer.clear();
                state.clear_epoch += 1;
                state.bookmarks.clear();
                if state.paused.is_some() {
                    state.paused = Some(Vec::new());
                }
            }
            DeviceLogCommand::Retry | DeviceLogCommand::CurrentUser => {
                let state = self.states.entry(id).or_default();
                state.current_user = *command == DeviceLogCommand::CurrentUser;
                state.problem = None;
                self.restart(cx);
            }
            DeviceLogCommand::Level(level) => {
                self.minimum = *level;
                let index = LogLevel::ALL
                    .iter()
                    .position(|candidate| candidate == level)
                    .unwrap_or_default();
                if self
                    .level
                    .read(cx)
                    .selected_index(cx)
                    .is_none_or(|selected| selected.row != index)
                {
                    self.level.update(cx, |select, cx| {
                        select.set_selected_index(
                            Some(gpui_kit::component::IndexPath::new(index)),
                            window,
                            cx,
                        )
                    });
                }
            }
            DeviceLogCommand::Follow(follow) => self.follow = *follow,
            DeviceLogCommand::Export => self.export(window, cx),
            DeviceLogCommand::Filters => {
                if let Some(state) = self.state()
                    && let Some(source) = state.source
                {
                    options::open_filters(
                        state.options.clone(),
                        source,
                        self.dispatch.clone(),
                        window,
                        cx,
                    );
                }
            }
            DeviceLogCommand::ApplyOptions(options) => {
                let state = self.states.entry(id).or_default();
                if !options.validate(state.source == Some(Source::Android)) {
                    return;
                }
                state.options = options.clone();
                state.buffer = LogBuffer::default();
                state.bookmarks.clear();
                state.paused = None;
                state.problem = None;
                self.restart(cx);
            }
            DeviceLogCommand::OpenService(_) => return,
            DeviceLogCommand::Bookmark(line) => {
                let state = self.states.entry(id).or_default();
                if !state.bookmarks.remove(line) {
                    state.bookmarks.insert(*line);
                }
            }
            DeviceLogCommand::Bookmarks => self.bookmarks_only = !self.bookmarks_only,
            DeviceLogCommand::Copy(line) => {
                if let Some(line) = self.visible.iter().find(|candidate| candidate.id == *line) {
                    cx.write_to_clipboard(ClipboardItem::new_string(line.text.to_string()));
                }
            }
            DeviceLogCommand::Diagnostics => {
                if let Some(state) = self.state()
                    && let Some(source) = state.source
                    && self.report_task.is_none()
                {
                    let name = self
                        .target
                        .as_ref()
                        .and_then(|target| self.store.read(cx).host(target.host))
                        .map(|host| host.name.clone())
                        .unwrap_or_default();
                    report::open_report(
                        source == Source::Android,
                        name,
                        self.dispatch.clone(),
                        window,
                        cx,
                    );
                }
            }
            DeviceLogCommand::CollectReport(options) => {
                self.collect_report(options.clone(), window, cx)
            }
            DeviceLogCommand::CancelReport => {
                self.report_task = None;
                self.report_progress = None;
                self.status = Some(t!("device_logs.report.cancelled"));
                self.restart(cx);
            }
            DeviceLogCommand::Search
            | DeviceLogCommand::SearchNext
            | DeviceLogCommand::SearchPrevious
            | DeviceLogCommand::ClearSearch
            | DeviceLogCommand::ToggleSearchRegex
            | DeviceLogCommand::ToggleSearchCaseSensitive
            | DeviceLogCommand::ToggleMatchingLines => {}
        }
        self.refresh_visible();
        self.follow_latest();
        cx.notify();
    }

    fn refresh_visible(&mut self) {
        self.refresh_search();
    }

    fn follow_latest(&self) {
        if self.follow
            && !self.search.has_query()
            && self.state().is_some_and(|state| state.paused.is_none())
        {
            let count = self.visible.len();
            if count > 0 {
                self.scroll
                    .scroll_to_item(count - 1, ScrollStrategy::Bottom);
            }
        }
    }

    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let lines = self.visible.clone();
        if lines.is_empty() {
            return;
        }
        let mut bytes = lines
            .iter()
            .map(|line| line.text.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        bytes.push('\n');
        let name = format!(
            "device-log-{}.log",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        let receiver = cx.prompt_for_new_path(
            &dirs::download_dir().unwrap_or_else(std::env::temp_dir),
            Some(&name),
        );
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let result = cx
                .background_spawn(async move { std::fs::write(path, bytes) })
                .await;
            this.update(cx, |this, cx| {
                this.status = Some(match result {
                    Ok(()) => t!("device_logs.exported"),
                    Err(error) => t!("device_logs.export_failed", error = error),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn collect_report(
        &mut self,
        options: ReportOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.report_task.is_some() {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some(source) = self.state().and_then(|state| state.source) else {
            return;
        };
        let source = if source == Source::Android {
            Source::Android
        } else {
            Source::Kernel
        };
        let current_user = self.state().is_some_and(|state| state.current_user);
        let Some(generation) = target
            .exec
            .view
            .upgrade()
            .map(|view| view.read(cx).generation(cx))
        else {
            return;
        };
        let name = self
            .store
            .read(cx)
            .host(target.host)
            .map(|host| host.name.to_string())
            .unwrap_or_default();
        let filename = format!(
            "diagnostic-{}.zip",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        let receiver = cx.prompt_for_new_path(
            &dirs::download_dir().unwrap_or_else(std::env::temp_dir),
            Some(&filename),
        );
        let connector = self.connector.clone();
        let store = self.store.clone();
        let displayed = self
            .visible
            .iter()
            .map(|line| line.text.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        self.report_progress = Some(t!("device_logs.report.choosing"));
        let guard = Cancel(Arc::new(AtomicBool::new(false)));
        self.report_task = Some(cx.spawn_in(window, async move |this, cx| {
            let guard = guard;
            let Ok(Ok(Some(destination))) = receiver.await else {
                this.update(cx, |this, cx| {
                    this.report_task = None;
                    this.report_progress = None;
                    cx.notify();
                })
                .ok();
                return;
            };
            this.update(cx, |this, cx| {
                this.capture = None;
                cx.notify();
            })
            .ok();
            let outcome: Result<(usize, usize), String> = async {
                let mut link =
                    diagnostic_link(&target, connector.clone(), store.clone(), &guard.0, cx)
                        .await
                        .map_err(problem_text)?;
                let route = link
                    .as_ref()
                    .map_or_else(|| "SSH".into(), |link| link.label());
                let mut results = Vec::new();
                let mut files = Vec::new();
                let privilege = if target.settings.elevated && !current_user {
                    match read(
                        &target.exec,
                        &mut link,
                        commands::access_probe(source, &LogOptions::default()),
                        &guard.0,
                        cx,
                    )
                    .await
                    {
                        Ok(output) => match commands::privilege(&output) {
                            Some(privilege) => privilege,
                            None => {
                                results.push(ReportResult {
                                    name: "privilege".into(),
                                    status: "failed".into(),
                                    detail: t!("device_logs.elevation_failed").into(),
                                });
                                Privilege::User
                            }
                        },
                        Err(problem) => {
                            results.push(ReportResult {
                                name: "privilege".into(),
                                status: "failed".into(),
                                detail: problem_text(problem),
                            });
                            Privilege::User
                        }
                    }
                } else {
                    Privilege::User
                };
                for item in report::items(source, privilege, &options) {
                    if !report_connected(&target, generation, cx)? {
                        return Err(t!("device_logs.not_connected").into());
                    }
                    this.update(cx, |this, cx| {
                        this.report_progress =
                            Some(t!("device_logs.report.progress", item = item.name));
                        cx.notify();
                    })
                    .ok();
                    let output = read(
                        &target.exec,
                        &mut link,
                        report::bounded(
                            &item.command,
                            if item.screenshot {
                                3 * 1024 * 1024
                            } else {
                                524288
                            },
                        ),
                        &guard.0,
                        cx,
                    )
                    .await
                    .map_err(problem_text);
                    let truncated = !item.screenshot
                        && output.as_ref().is_ok_and(|output| {
                            output
                                .split_once('\n')
                                .is_some_and(|(_, text)| text.len() >= 524288)
                        });
                    let data = output.and_then(|output| {
                        let text = report::unpack(&output)?;
                        if item.screenshot {
                            use base64::Engine as _;
                            let compact = text
                                .chars()
                                .filter(|value| !value.is_whitespace())
                                .collect::<String>();
                            let bytes = base64::engine::general_purpose::STANDARD
                                .decode(compact)
                                .map_err(|error| error.to_string())?;
                            if !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                                || !bytes.ends_with(b"\0\0\0\0IEND\xaeB`\x82")
                            {
                                return Err(t!("device_logs.report.invalid_image").into());
                            }
                            Ok(bytes)
                        } else if item.name.ends_with(".log") {
                            let text = commands::read_output(text)?;
                            let source = if item.name == "system-journal.log" {
                                Source::Journal
                            } else {
                                source
                            };
                            let mut buffer = LogBuffer::default();
                            buffer.ingest(source, text, false);
                            Ok(buffer
                                .lines()
                                .iter()
                                .map(|line| line.text.as_ref())
                                .collect::<Vec<_>>()
                                .join("\n")
                                .into_bytes())
                        } else {
                            Ok(text.as_bytes().to_vec())
                        }
                    });
                    match data {
                        Ok(bytes) => {
                            results.push(ReportResult {
                                name: item.name.into(),
                                status: if truncated { "truncated" } else { "ok" }.into(),
                                detail: if truncated {
                                    t!("device_logs.report.truncated").into()
                                } else {
                                    String::new()
                                },
                            });
                            files.push(ReportFile {
                                name: item.name.into(),
                                bytes,
                            });
                        }
                        Err(error) => results.push(ReportResult {
                            name: item.name.into(),
                            status: "failed".into(),
                            detail: error,
                        }),
                    }
                }
                if options.logs {
                    files.push(ReportFile {
                        name: "displayed-logs.log".into(),
                        bytes: displayed.into_bytes(),
                    });
                }
                if source == Source::Android && options.bugreport {
                    if !report_connected(&target, generation, cx)? {
                        return Err(t!("device_logs.not_connected").into());
                    }
                    this.update(cx, |this, cx| {
                        this.report_progress = Some(t!("device_logs.report.bugreport_progress"));
                        cx.notify();
                    })
                    .ok();
                    let attachment = async {
                        let mut device = link.as_ref().and_then(|link| link.adb_connection());
                        if device.is_none() {
                            let connector = connector
                                .ok_or_else(|| t!("device_logs.report.adb_required").to_string())?;
                            let identity = read(
                                &target.exec,
                                &mut link,
                                crate::serial::files::NETWORK_COMMAND.into(),
                                &guard.0,
                                cx,
                            )
                            .await
                            .map_err(problem_text)?;
                            let mut settings = target.serial.clone().unwrap_or_default();
                            settings.channel = crate::host::MonitorChannel::Adb;
                            let cancel = guard.0.clone();
                            device = cx
                                .background_spawn(async move {
                                    connector
                                        .connect(&identity, &settings, &[], &cancel)
                                        .and_then(|link| {
                                            link.and_then(|link| link.adb_connection()).ok_or_else(
                                                || t!("device_logs.report.adb_required").into(),
                                            )
                                        })
                                })
                                .await
                                .ok();
                        }
                        let device = device
                            .ok_or_else(|| t!("device_logs.report.adb_required").to_string())?;
                        let cancel = guard.0.clone();
                        cx.background_spawn(async move {
                            let temporary =
                                tempfile::tempdir().map_err(|error| error.to_string())?;
                            let file = temporary.path().join("bugreport.zip");
                            device
                                .collect_bugreport(&file, &cancel)
                                .map_err(|error| error.to_string())?;
                            if std::fs::metadata(&file)
                                .map_err(|error| error.to_string())?
                                .len()
                                > 128 * 1024 * 1024
                            {
                                return Err(t!("device_logs.report.bugreport_large").to_string());
                            }
                            std::fs::read(file).map_err(|error| error.to_string())
                        })
                        .await
                    }
                    .await;
                    match attachment {
                        Ok(bytes) => {
                            files.push(ReportFile {
                                name: "android-bugreport.zip".into(),
                                bytes,
                            });
                            results.push(ReportResult {
                                name: "android-bugreport.zip".into(),
                                status: "ok".into(),
                                detail: String::new(),
                            });
                        }
                        Err(error) => results.push(ReportResult {
                            name: "android-bugreport.zip".into(),
                            status: "failed".into(),
                            detail: error,
                        }),
                    }
                }
                if !report_connected(&target, generation, cx)? {
                    return Err(t!("device_logs.not_connected").into());
                }
                let success = results
                    .iter()
                    .filter(|result| result.status == "ok")
                    .count();
                let incomplete = results.len() - success;
                let cancel = guard.0.clone();
                cx.background_spawn(async move {
                    if cancel.load(Ordering::Acquire) {
                        return Err(t!("device_logs.report.cancelled").to_string());
                    }
                    report::write_zip(&destination, &name, &route, &files, &results, &cancel)
                })
                .await?;
                Ok((success, incomplete))
            }
            .await;
            this.update(cx, |this, cx| {
                this.report_task = None;
                this.report_progress = None;
                this.status = Some(match outcome {
                    Ok((success, incomplete)) => t!(
                        "device_logs.report.saved",
                        success = success,
                        incomplete = incomplete
                    ),
                    Err(error) => t!("device_logs.report.failed", error = error),
                });
                this.restart(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        self.capture = None;
        let Some(target) = self
            .target
            .clone()
            .filter(|target| self.active && target.settings.enabled)
        else {
            return;
        };
        let connector = self.connector.clone();
        let store = self.store.clone();
        let guard = Cancel(Arc::new(AtomicBool::new(false)));
        self.capture = Some(cx.spawn(async move |this, cx| {
            let guard = guard;
            let mut link: Option<Box<dyn MonitorLink>> = None;
            let mut session: Option<(Source, Privilege)> = None;
            let mut generation = None;
            loop {
                if guard.0.load(Ordering::Acquire) {
                    break;
                }
                let request = this.update(cx, |this, cx| {
                    let view = target.exec.view.upgrade()?;
                    let view = view.read(cx);
                    let current = view.generation(cx);
                    let running = view.lifecycle(cx) == TerminalLifecycle::Running;
                    let state = this.states.entry(target.exec.terminal).or_default();
                    let changed = state.generation != Some(current);
                    if changed {
                        let options = state.options.clone();
                        *state = LogState {
                            generation: Some(current),
                            settings: target.settings.clone(),
                            options,
                            ..LogState::default()
                        };
                    }
                    let request = (
                        current,
                        running,
                        state.current_user,
                        state.clear_epoch,
                        state.options.clone(),
                    );
                    if changed {
                        this.refresh_visible();
                        cx.notify();
                    }
                    Some(request)
                });
                let Ok(Some((current, running, current_user, epoch, options))) = request else {
                    break;
                };
                if generation != Some(current) {
                    link = None;
                    session = None;
                    generation = Some(current);
                }
                if !running {
                    link = None;
                    session = None;
                    this.update(cx, |this, cx| {
                        this.states.entry(target.exec.terminal).or_default().problem =
                            Some(Problem::NotConnected);
                        if this.state().is_some_and(|state| state.paused.is_none()) {
                            this.refresh_visible();
                        }
                        this.follow_latest();
                        cx.notify();
                    })
                    .ok();
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    continue;
                }
                let outcome: Result<(Source, Privilege, String, String), Problem> = async {
                    if target.serial.is_some() && link.is_none() {
                        link = diagnostic_link(
                            &target,
                            connector.clone(),
                            store.clone(),
                            &guard.0,
                            cx,
                        )
                        .await?;
                    }
                    if session.is_none() {
                        let output = read(
                            &target.exec,
                            &mut link,
                            commands::PROBE.into(),
                            &guard.0,
                            cx,
                        )
                        .await?;
                        let source = commands::effective_source(
                            commands::source(&output).ok_or(Problem::Unsupported)?,
                            &options,
                        );
                        this.update(cx, |this, cx| {
                            this.states.entry(target.exec.terminal).or_default().source =
                                Some(source);
                            cx.notify();
                        })
                        .ok();
                        let privilege = if target.settings.elevated && !current_user {
                            let output = read(
                                &target.exec,
                                &mut link,
                                commands::access_probe(source, &options),
                                &guard.0,
                                cx,
                            )
                            .await?;
                            commands::privilege(&output).ok_or(Problem::Elevation)?
                        } else {
                            Privilege::User
                        };
                        session = Some((source, privilege));
                    }
                    let (source, privilege) = session.unwrap();
                    let output = read(
                        &target.exec,
                        &mut link,
                        commands::query_command(source, privilege, &options),
                        &guard.0,
                        cx,
                    )
                    .await?;
                    let data = commands::read_output(&output)
                        .map_err(Problem::Read)?
                        .to_string();
                    let route = link
                        .as_ref()
                        .map_or_else(|| "SSH".into(), |link| link.label());
                    Ok((source, privilege, route, data))
                }
                .await;
                let failed = outcome.is_err();
                let applied = this.update(cx, |this, cx| {
                    let current_generation = target
                        .exec
                        .view
                        .upgrade()
                        .map(|view| view.read(cx).generation(cx));
                    if current_generation != Some(current)
                        || this.target.as_ref() != Some(&target)
                        || !this.active
                    {
                        return false;
                    }
                    let state = this.states.entry(target.exec.terminal).or_default();
                    match outcome {
                        Ok((source, privilege, route, data)) => {
                            state.source = Some(source);
                            state.elevated = privilege != Privilege::User;
                            state.route = route;
                            state.problem = None;
                            state.app_waiting = source == Source::Android
                                && !state.options.object.is_empty()
                                && data
                                    .lines()
                                    .any(|line| line.trim() == "__SHELLLF_LOG_PIDS__");
                            state
                                .buffer
                                .ingest(source, &data, epoch != state.clear_epoch);
                            let retained: HashSet<_> =
                                state.buffer.lines().iter().map(|line| line.id).collect();
                            state.bookmarks.retain(|id| retained.contains(id));
                        }
                        Err(problem) => state.problem = Some(problem),
                    }
                    if this.state().is_some_and(|state| state.paused.is_none()) {
                        this.refresh_visible();
                    }
                    this.follow_latest();
                    cx.notify();
                    true
                });
                match applied {
                    Err(_) => break,
                    Ok(false) => {
                        link = None;
                        session = None;
                        continue;
                    }
                    Ok(true) => {}
                }
                if failed {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        }));
    }
}

fn problem_text(problem: Problem) -> String {
    match problem {
        Problem::NotConnected => t!("device_logs.not_connected").into(),
        Problem::Busy => t!("device_logs.busy").into(),
        Problem::NoChannel => t!("device_logs.no_channel").into(),
        Problem::Unsupported => t!("device_logs.unsupported").into(),
        Problem::Elevation => t!("device_logs.elevation_failed").into(),
        Problem::Read(error) => error,
    }
}

fn report_connected(
    target: &LogTarget,
    generation: u64,
    cx: &mut AsyncApp,
) -> Result<bool, String> {
    target
        .exec
        .view
        .update(cx, |view, cx| {
            view.generation(cx) == generation && view.lifecycle(cx) == TerminalLifecycle::Running
        })
        .map_err(|_| t!("device_logs.not_connected").to_string())
}

async fn diagnostic_link(
    target: &LogTarget,
    connector: Option<Arc<dyn MonitorConnector>>,
    store: Entity<HostStore>,
    cancel: &Arc<AtomicBool>,
    cx: &mut AsyncApp,
) -> Result<Option<Box<dyn MonitorLink>>, Problem> {
    let Some(mut settings) = target.serial.clone() else {
        return Ok(None);
    };
    let identity = terminal_command(
        &target.exec,
        crate::serial::files::NETWORK_COMMAND.into(),
        true,
        cx,
    )
    .await?;
    let connector = connector.ok_or(Problem::NoChannel)?;
    if settings.channel == crate::host::MonitorChannel::Serial {
        settings.channel = crate::host::MonitorChannel::Auto;
    }
    let candidates = store.read_with(cx, |store, _| {
        store
            .hosts()
            .iter()
            .filter(|host| host.is_ssh())
            .filter_map(|host| {
                store
                    .login(host.id)
                    .map(|login| (host.public_id.as_str().to_owned(), login))
            })
            .collect::<Vec<_>>()
    });
    let cancel = cancel.clone();
    let link = cx
        .background_spawn(
            async move { connector.connect(&identity, &settings, &candidates, &cancel) },
        )
        .await
        .map_err(Problem::Read)?;
    link.map(Some).ok_or(Problem::NoChannel)
}

async fn terminal_command(
    target: &ExecTarget,
    command: String,
    monitor: bool,
    cx: &mut AsyncApp,
) -> Result<String, Problem> {
    let reply = target
        .view
        .update(cx, |view, cx| {
            if monitor {
                view.monitor(command, cx)
            } else {
                view.exec(command, cx)
            }
        })
        .ok()
        .flatten();
    let Some(reply) = reply else {
        return Err(Problem::NotConnected);
    };
    match exec_answer(reply, cx).await {
        Some(Ok(output)) => Ok(output),
        Some(Err(error)) if error == crate::terminal::MONITOR_PAUSED => Err(Problem::Busy),
        Some(Err(error)) => Err(Problem::Read(error)),
        None => Err(Problem::NotConnected),
    }
}

async fn read(
    target: &ExecTarget,
    link: &mut Option<Box<dyn MonitorLink>>,
    command: String,
    cancel: &Arc<AtomicBool>,
    cx: &mut AsyncApp,
) -> Result<String, Problem> {
    if let Some(mut current) = link.take() {
        let cancel = cancel.clone();
        let (current, result) = cx
            .background_spawn(async move {
                let result = current.sample(&command, &cancel);
                (current, result)
            })
            .await;
        *link = Some(current);
        result.map_err(Problem::Read)
    } else {
        terminal_command(target, command, false, cx).await
    }
}

impl Render for DeviceLogPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = self.settings.read(cx).settings().device_logs.font_size;
        let font_size =
            rems(f32::from(size) / DEFAULT_INTERFACE_FONT_SIZE).to_pixels(window.rem_size());
        let lines = self.visible.clone();
        let count = lines.len();
        let state = self.state();
        let paused = state.is_some_and(|state| state.paused.is_some());
        let problem = state.and_then(|state| state.problem.clone());
        let limited = state.is_some_and(|state| state.buffer.trimmed || state.buffer.gap);
        let source = state.and_then(|state| state.source);
        let generation = state.and_then(|state| state.generation).unwrap_or_default();
        let terminal_id = self
            .target
            .as_ref()
            .map_or(0, |target| target.exec.terminal.0);
        let session_id =
            SharedString::from(format!("device-log-session-{terminal_id}-{generation}"));
        let fallback_allowed = self
            .target
            .as_ref()
            .is_some_and(|target| target.settings.elevated)
            && !state.is_some_and(|state| state.current_user);
        let loading = source.is_none() && problem.is_none();
        let route = state.map_or("", |state| state.route.as_str()).to_string();
        let elevated = state.is_some_and(|state| state.elevated);
        let source_label = source.map(|source| {
            t!(
                "device_logs.source",
                source = match source {
                    Source::Android => "logcat",
                    Source::Kernel => "dmesg",
                    Source::Journal => "journalctl",
                    Source::File => "tail",
                },
                route = route,
                permission = if elevated {
                    t!("device_logs.admin")
                } else {
                    t!("device_logs.user")
                }
            )
        });
        let bookmarks = state
            .map(|state| state.bookmarks.clone())
            .unwrap_or_default();
        let dispatch = self.dispatch.clone();
        let filters_active = state.is_some_and(|state| state.options != LogOptions::default());
        let filter_object = state
            .map(|state| state.options.object.clone())
            .filter(|object| !object.is_empty());
        let app_waiting = state.is_some_and(|state| state.app_waiting);
        let search_regex = self.search.compiled.clone();
        let selected_search = self.search.selected;
        let search_label = if !self.search.has_query() || self.search.error.is_some() {
            None
        } else if self.search.matches.is_empty() {
            Some(t!("device_logs.search.no_results"))
        } else {
            Some(
                match self.search.selected.and_then(|id| {
                    self.search
                        .matches
                        .iter()
                        .position(|candidate| *candidate == id)
                }) {
                    Some(index) => format!("{}/{}", index + 1, self.search.matches.len()),
                    None => t!(
                        "device_logs.search.count",
                        count = self.search.matches.len()
                    )
                    .to_string(),
                }
                .into(),
            )
        };
        let search_error = self.search.error.clone();
        let action = |id: &'static str, label: SharedString, command: DeviceLogCommand| {
            let dispatch = self.dispatch.clone();
            let icon = match &command {
                DeviceLogCommand::Clear => Some(CatalogIcon::Eraser),
                DeviceLogCommand::Export => Some(CatalogIcon::Download),
                DeviceLogCommand::Filters => Some(CatalogIcon::ListFilter),
                DeviceLogCommand::Bookmarks => Some(CatalogIcon::Bookmark),
                DeviceLogCommand::Diagnostics => Some(CatalogIcon::FileText),
                _ => None,
            };
            Button::new(id)
                .small()
                .when_some(icon, |button, icon| button.ghost().icon(Icon::new(icon)))
                .when(icon.is_none(), |button| button.label(label.clone()))
                .tooltip(label.clone())
                .accessibility_label(label)
                .on_click(move |_, window, cx| {
                    dispatch.dispatch_action(&DeviceLogAction(command.clone()), window, cx)
                })
        };
        let follow_dispatch = self.dispatch.clone();
        let search_option = |id, label: SharedString, command: DeviceLogCommand, checked| {
            let dispatch = self.dispatch.clone();
            Checkbox::new(id)
                .small()
                .w_auto()
                .flex_shrink_0()
                .label(label)
                .checked(checked)
                .on_change(move |_, window, cx| {
                    dispatch.dispatch_action(&DeviceLogAction(command.clone()), window, cx)
                })
        };
        v_flex()
            .id("device-logs")
            .test_support()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(
                v_flex()
                    .px_3()
                    .py_3()
                    .gap_3()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .justify_between()
                            .child(
                                action(
                                    "device-logs-split",
                                    if self.split {
                                        t!("device_logs.split_off")
                                    } else {
                                        t!("device_logs.split")
                                    },
                                    DeviceLogCommand::ToggleSplit,
                                )
                                .outline()
                                .selected(self.split)
                                .toggled(self.split),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        action(
                                            "device-logs-font-smaller",
                                            "A−".into(),
                                            DeviceLogCommand::FontSize(size.saturating_sub(1)),
                                        )
                                        .ghost()
                                        .tooltip(t!("device_logs.font_smaller"))
                                        .accessibility_label(t!("device_logs.font_smaller"))
                                        .disabled(size <= 12),
                                    )
                                    .child(
                                        div()
                                            .id("device-logs-font-size")
                                            .test_support()
                                            .aria_label(t!("device_logs.font_size", size = size))
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(size.to_string()),
                                    )
                                    .child(
                                        action(
                                            "device-logs-font-larger",
                                            "A+".into(),
                                            DeviceLogCommand::FontSize(size.saturating_add(1)),
                                        )
                                        .ghost()
                                        .tooltip(t!("device_logs.font_larger"))
                                        .accessibility_label(t!("device_logs.font_larger"))
                                        .disabled(size >= 24),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                action(
                                    "device-logs-pause",
                                    if paused {
                                        t!("device_logs.resume")
                                    } else {
                                        t!("device_logs.pause")
                                    },
                                    DeviceLogCommand::Pause,
                                )
                                .disabled(!paused && (loading || problem.is_some())),
                            )
                            .child(
                                Checkbox::new("device-logs-follow")
                                    .small()
                                    .w_auto()
                                    .flex_shrink_0()
                                    .checked(self.follow)
                                    .label(t!("device_logs.follow"))
                                    .on_change(move |follow, window, cx| {
                                        follow_dispatch.dispatch_action(
                                            &DeviceLogAction(DeviceLogCommand::Follow(*follow)),
                                            window,
                                            cx,
                                        )
                                    }),
                            )
                            .child(search_option(
                                "device-logs-search-regex",
                                t!("device_logs.search.regex"),
                                DeviceLogCommand::ToggleSearchRegex,
                                self.search.regex,
                            ))
                            .child(search_option(
                                "device-logs-search-case",
                                t!("device_logs.search.case_sensitive"),
                                DeviceLogCommand::ToggleSearchCaseSensitive,
                                self.search.case_sensitive,
                            ))
                            .child(search_option(
                                "device-logs-search-lines",
                                t!("device_logs.search.matching_lines"),
                                DeviceLogCommand::ToggleMatchingLines,
                                self.search.matching_lines,
                            ))
                            .child(
                                action(
                                    "device-logs-filters",
                                    t!("device_logs.filters"),
                                    DeviceLogCommand::Filters,
                                )
                                .selected(filters_active)
                                .disabled(source.is_none()),
                            )
                            .child(
                                action(
                                    "device-logs-bookmarks",
                                    t!("device_logs.bookmarks"),
                                    DeviceLogCommand::Bookmarks,
                                )
                                .selected(self.bookmarks_only)
                                .toggled(self.bookmarks_only),
                            )
                            .child(
                                action(
                                    "device-logs-diagnostics",
                                    t!("device_logs.report.title"),
                                    DeviceLogCommand::Diagnostics,
                                )
                                .disabled(source.is_none() || self.report_task.is_some()),
                            )
                            .child(
                                action(
                                    "device-logs-export",
                                    t!("device_logs.export"),
                                    DeviceLogCommand::Export,
                                )
                                .disabled(count == 0),
                            )
                            .child(
                                action(
                                    "device-logs-clear",
                                    t!("device_logs.clear"),
                                    DeviceLogCommand::Clear,
                                )
                                .tooltip(t!("device_logs.clear_note")),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .min_w_0()
                            .child(
                                Input::new(&self.keyword)
                                    .id("device-logs-keyword")
                                    .small()
                                    .flex_1()
                                    .min_w_0()
                                    .prefix(
                                        Icon::new(gpui_kit::component::IconName::Search).small(),
                                    )
                                    .aria_label(t!("device_logs.keyword")),
                            )
                            .when(self.search.has_query(), |view| {
                                let dispatch = self.dispatch.clone();
                                view.child(
                                    Button::new("device-logs-search-clear")
                                        .ghost()
                                        .xsmall()
                                        .icon(gpui_kit::component::IconName::Close)
                                        .tooltip(t!("device_logs.search.clear"))
                                        .accessibility_label(t!("device_logs.search.clear"))
                                        .on_click(move |_, window, cx| {
                                            dispatch.dispatch_action(
                                                &DeviceLogAction(DeviceLogCommand::ClearSearch),
                                                window,
                                                cx,
                                            )
                                        }),
                                )
                            })
                            .when_some(search_label, |view, label| {
                                view.child(
                                    div()
                                        .id("device-logs-search-count")
                                        .test_support()
                                        .aria_label(label.clone())
                                        .flex_none()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(label),
                                )
                            })
                            .child({
                                let dispatch = self.dispatch.clone();
                                Button::new("device-logs-search-previous")
                                    .ghost()
                                    .xsmall()
                                    .icon(gpui_kit::component::IconName::ChevronUp)
                                    .tooltip(t!("device_logs.search.previous"))
                                    .accessibility_label(t!("device_logs.search.previous"))
                                    .disabled(self.search.matches.is_empty())
                                    .on_click(move |_, window, cx| {
                                        dispatch.dispatch_action(
                                            &DeviceLogAction(DeviceLogCommand::SearchPrevious),
                                            window,
                                            cx,
                                        )
                                    })
                            })
                            .child({
                                let dispatch = self.dispatch.clone();
                                Button::new("device-logs-search-next")
                                    .ghost()
                                    .xsmall()
                                    .icon(gpui_kit::component::IconName::ChevronDown)
                                    .tooltip(t!("device_logs.search.next"))
                                    .accessibility_label(t!("device_logs.search.next"))
                                    .disabled(self.search.matches.is_empty())
                                    .on_click(move |_, window, cx| {
                                        dispatch.dispatch_action(
                                            &DeviceLogAction(DeviceLogCommand::SearchNext),
                                            window,
                                            cx,
                                        )
                                    })
                            }),
                    )
                    .child(
                        h_flex().flex_wrap().gap_2().child(
                            Select::new(&self.level)
                                .id("device-logs-level")
                                .w_24()
                                .small()
                                .placeholder(t!("device_logs.level.all")),
                        ),
                    )
                    .when_some(search_error, |view, error| {
                        view.child(
                            div()
                                .id("device-logs-search-error")
                                .test_support()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child(t!("device_logs.search.invalid_regex"))
                                .tooltip(move |window, cx| {
                                    Tooltip::new(error.clone()).build(window, cx)
                                }),
                        )
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                div()
                                    .flex_none()
                                    .child(t!("device_logs.count", count = count)),
                            )
                            .when_some(source_label, |view, label| {
                                view.child(
                                    div()
                                        .id("device-logs-source")
                                        .test_support()
                                        .flex_1()
                                        .min_w_0()
                                        .aria_label(label.clone())
                                        .truncate()
                                        .tooltip({
                                            let label = label.clone();
                                            move |window, cx| {
                                                Tooltip::new(label.clone()).build(window, cx)
                                            }
                                        })
                                        .child(label),
                                )
                            }),
                    )
                    .when_some(filter_object, |view, object| {
                        view.child(
                            div()
                                .id("device-logs-object")
                                .test_support()
                                .aria_label(object.clone())
                                .text_xs()
                                .truncate()
                                .child(object),
                        )
                    })
                    .when(app_waiting, |view| {
                        view.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("device_logs.app_waiting")),
                        )
                    })
                    .when(limited, |view| {
                        view.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().warning)
                                .child(t!("device_logs.limited")),
                        )
                    })
                    .when(loading, |view| {
                        view.child(
                            div()
                                .id("device-logs-loading")
                                .test_support()
                                .text_sm()
                                .child(t!("device_logs.loading")),
                        )
                    })
                    .when(!loading && count == 0 && problem.is_none(), |view| {
                        view.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("device_logs.empty")),
                        )
                    })
                    .when(paused, |view| {
                        view.child(div().text_xs().child(t!("device_logs.paused")))
                    })
                    .when_some(problem, |view, problem| {
                        let elevated_error = fallback_allowed
                            && matches!(problem, Problem::Elevation | Problem::Read(_));
                        let message = match problem {
                            Problem::NotConnected => t!("device_logs.not_connected"),
                            Problem::Busy => t!("device_logs.busy"),
                            Problem::NoChannel => t!("device_logs.no_channel"),
                            Problem::Unsupported => t!("device_logs.unsupported"),
                            Problem::Elevation => t!("device_logs.elevation_failed"),
                            Problem::Read(error) => t!("device_logs.read_failed", error = error),
                        };
                        view.child(
                            div()
                                .id("device-logs-error")
                                .test_support()
                                .aria_label(message.clone())
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(message),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(action(
                                    "device-logs-retry",
                                    t!("device_logs.retry"),
                                    DeviceLogCommand::Retry,
                                ))
                                .when(elevated_error, |row| {
                                    row.child(action(
                                        "device-logs-current-user",
                                        t!("device_logs.current_user"),
                                        DeviceLogCommand::CurrentUser,
                                    ))
                                }),
                        )
                    })
                    .when_some(self.status.clone(), |view, status| {
                        view.child(
                            div()
                                .id("device-logs-status")
                                .test_support()
                                .aria_label(status.clone())
                                .text_sm()
                                .child(status),
                        )
                    }),
            )
            .when_some(self.report_progress.clone(), |view, progress| {
                view.child(
                    h_flex()
                        .px_3()
                        .gap_2()
                        .child(
                            div()
                                .id("diagnostic-progress")
                                .test_support()
                                .text_sm()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(progress),
                        )
                        .child(action(
                            "diagnostic-stop",
                            t!("common.cancel"),
                            DeviceLogCommand::CancelReport,
                        )),
                )
            })
            .child(
                div()
                    .id(session_id)
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .bg(cx.theme().background)
                    .child(
                        uniform_list(
                            ("device-logs-list", u64::from(size)),
                            count,
                            move |range, _, cx| {
                                range
                                    .map(|index| {
                                        let line = &lines[index];
                                        let text: SharedString = line.text.to_string().into();
                                        let tooltip: SharedString =
                                            text.chars().take(2000).collect::<String>().into();
                                        let mut preview = text
                                            .chars()
                                            .take(4096)
                                            .collect::<String>()
                                            .replace(['\r', '\n'], " ↵ ");
                                        if text.chars().nth(4096).is_some() {
                                            preview.push('…');
                                        }
                                        let bookmark_dispatch = dispatch.clone();
                                        let copy_dispatch = dispatch.clone();
                                        let id = line.id;
                                        let highlights = search_regex
                                            .as_ref()
                                            .map(|regex| spans(regex, &preview))
                                            .unwrap_or_default();
                                        let text_element = if highlights.is_empty() {
                                            div().child(preview).into_any_element()
                                        } else {
                                            let highlight = HighlightStyle {
                                                background_color: Some(
                                                    if selected_search == Some(id) {
                                                        cx.theme().primary.opacity(0.35)
                                                    } else {
                                                        cx.theme().warning.opacity(0.35)
                                                    },
                                                ),
                                                font_weight: Some(FontWeight::BOLD),
                                                ..HighlightStyle::default()
                                            };
                                            StyledText::new(preview.clone())
                                                .with_highlights(
                                                    highlights
                                                        .into_iter()
                                                        .map(|range| (range, highlight)),
                                                )
                                                .into_any_element()
                                        };
                                        h_flex()
                                            .id(("device-log-line", line.id))
                                            .test_support()
                                            .aria_label(text.clone())
                                            .px_3()
                                            .py_1p5()
                                            .text_size(font_size)
                                            .line_height(relative(1.5))
                                            .gap_2()
                                            .border_b_1()
                                            .border_color(cx.theme().border.opacity(0.5))
                                            .when(selected_search == Some(id), |row| {
                                                row.bg(cx.theme().list_active)
                                            })
                                            .child(
                                                Button::new(("log-bookmark", id))
                                                    .ghost()
                                                    .xsmall()
                                                    .label(if bookmarks.contains(&id) {
                                                        "★"
                                                    } else {
                                                        "☆"
                                                    })
                                                    .tooltip(t!("device_logs.bookmark"))
                                                    .accessibility_label(t!("device_logs.bookmark"))
                                                    .on_click(move |_, window, cx| {
                                                        bookmark_dispatch.dispatch_action(
                                                            &DeviceLogAction(
                                                                DeviceLogCommand::Bookmark(id),
                                                            ),
                                                            window,
                                                            cx,
                                                        )
                                                    }),
                                            )
                                            .child(
                                                Button::new(("log-copy", id))
                                                    .ghost()
                                                    .xsmall()
                                                    .icon(gpui_kit::component::IconName::Copy)
                                                    .tooltip(t!("device_logs.copy"))
                                                    .accessibility_label(t!("device_logs.copy"))
                                                    .on_click(move |_, window, cx| {
                                                        copy_dispatch.dispatch_action(
                                                            &DeviceLogAction(
                                                                DeviceLogCommand::Copy(id),
                                                            ),
                                                            window,
                                                            cx,
                                                        )
                                                    }),
                                            )
                                            .child(
                                                div()
                                                    .id(("log-text", id))
                                                    .test_support()
                                                    .font_family(
                                                        cx.theme().mono_font_family.clone(),
                                                    )
                                                    .text_color(match line.level {
                                                        LogLevel::Error | LogLevel::Fatal => {
                                                            cx.theme().danger
                                                        }
                                                        LogLevel::Warning => cx.theme().warning,
                                                        LogLevel::Verbose | LogLevel::Debug => {
                                                            cx.theme().muted_foreground
                                                        }
                                                        _ => cx.theme().foreground,
                                                    })
                                                    .flex_none()
                                                    .whitespace_nowrap()
                                                    .tooltip(move |window, cx| {
                                                        Tooltip::new(tooltip.clone())
                                                            .build(window, cx)
                                                    })
                                                    .child(text_element),
                                            )
                                    })
                                    .collect::<Vec<_>>()
                            },
                        )
                        .with_horizontal_sizing_behavior(
                            ListHorizontalSizingBehavior::Unconstrained,
                        )
                        .with_width_from_item(
                            self.visible
                                .iter()
                                .enumerate()
                                .max_by_key(|(_, line)| line.text.len().min(4096))
                                .map(|(index, _)| index),
                        )
                        .track_scroll(&self.scroll)
                        .size_full(),
                    )
                    .scrollbar(
                        &self.scroll,
                        gpui_kit::component::scroll::ScrollbarAxis::Both,
                    ),
            )
    }
}
