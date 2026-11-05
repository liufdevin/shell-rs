//! ZMODEM in a terminal: `rz` and `sz` on the other end of the connection,
//! as Xshell and SecureCRT take them. The parser thread hands every chunk
//! of output to [`Zmodem`] before the emulator sees it. With no transfer
//! running it only looks for the header `rz` and `sz` open with, and
//! passes the rest on; during one, the bytes are the protocol's, and what
//! it answers goes back to the other end. The protocol itself is the
//! `zmodem2` crate: this is the wiring, the files, and the lines the
//! terminal shows of it.

use std::collections::VecDeque;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use zmodem2::{Action, Event, FileInfo, Position, Receiver, Sender};

use crate::i18n::t;
use crate::shared::format_bytes;

/// What `rz` and `sz` open with: a hex header, then its type, `01`
/// (ZRINIT) from `rz` waiting for files, or `00` (ZRQINIT) from `sz`
/// offering some.
const HEADER: &[u8] = b"**\x18B0";
const CAN: u8 = 0x18;
/// ZMODEM's escape, the same byte as CAN.
const ZDLE: u8 = CAN;
/// lrzsz's `rz` and `sz` give up on five CANs in a row, which is how the
/// other end cancels; so does this.
const CANCEL_RUN: usize = 5;
/// What cancels a transfer: ten CANs, then ten backspaces to rub them
/// out where they show. The same bytes as zmodem2's `abort` and lrzsz's
/// `canit()`.
const ABORT: [u8; 20] = [
    CAN, CAN, CAN, CAN, CAN, CAN, CAN, CAN, CAN, CAN, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
];
/// How long the other end may say nothing before it is asked again (the
/// handshake is sent again, or else nothing), and how many times.
const QUIET: Duration = Duration::from_secs(5);
const QUIET_TIMES: u32 = 6;
/// The progress line shows anew at most this often, when bytes move.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// Subpackets (of 1 KB) sent before waiting for the other end to
/// acknowledge them: what keeps an upload from piling up in the
/// connection's queue. It starts small and follows how long each lot takes
/// to be acknowledged: doubled under `FRAME_QUICK`, halved over
/// `FRAME_SLOW`, so a link with some latency gets the most, and a serial
/// line at 9600 baud (1 KB a second) lots it sends within a few seconds,
/// well inside `QUIET` and soon cancelled.
const WINDOW_FIRST: usize = 4;
const WINDOW_MOST: usize = 256;
const FRAME_QUICK: Duration = Duration::from_secs(1);
const FRAME_SLOW: Duration = Duration::from_secs(3);
/// Bad headers in a row tolerated before giving up.
const ERRORS: u32 = 10;
/// After this side stops a download, the sender streams on until it reads
/// the CANs, which lrzsz's `sz` does only now and then (within a few
/// hundred milliseconds). Its data is swallowed meanwhile, until the
/// output has been quiet this long, or this long has passed, or Ctrl+C is
/// pressed again. The CANs go once: sent again, they could land on the
/// shell once `sz` is gone, as typed text.
const DRAIN_QUIET: Duration = Duration::from_millis(500);
const DRAIN_AT_MOST: Duration = Duration::from_secs(600);
/// What a drain keeps of the sender's data at most, waiting for the end of
/// a subpacket: they are 1 KB, twice that escaped.
const DRAIN_KEPT: usize = 64 * 1024;
/// What the other end sends while the user chooses, kept for the answer.
const HELD_AT_MOST: usize = 64 * 1024;
/// How long files dropped on the terminal wait for the `rz` typed for
/// them to answer.
pub(crate) const DROPPED_FOR: Duration = Duration::from_secs(10);

/// Which way the files go: up to the other end, which runs `rz`, or down
/// from it, which runs `sz`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZmodemDirection {
    Upload,
    Download,
}

/// What the user chose for a transfer the other end started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZmodemReply {
    /// The files to upload.
    Files(Vec<PathBuf>),
    /// The folder to download into.
    Folder(PathBuf),
    /// Nothing: the transfer is cancelled.
    Declined,
}

/// What the UI thread tells the parser thread's [`Zmodem`].
#[derive(Debug)]
pub(crate) enum ZmodemControl {
    Reply(ZmodemReply),
    /// Ctrl+C in the terminal.
    Cancel,
    /// Files dropped on the terminal, which typed `rz` for them.
    Dropped(Vec<PathBuf>),
}

/// What a chunk of output, an answer or the time came to.
#[derive(Debug, Default)]
pub(crate) struct Fed {
    /// What the terminal shows, in order: the output the emulator takes,
    /// with the transfer's lines between. A chunk can hold the text
    /// before a header and the prompt after the transfer.
    pub shown: Vec<Shown>,
    /// For the other end.
    pub wire: Vec<u8>,
    /// The user has to choose files or a folder.
    pub ask: Option<ZmodemDirection>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Shown {
    Output(Vec<u8>),
    Note(Note),
}

impl Fed {
    fn output(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        match self.shown.last_mut() {
            Some(Shown::Output(output)) => output.extend_from_slice(bytes),
            _ => self.shown.push(Shown::Output(bytes.to_vec())),
        }
    }

    fn note(&mut self, note: Note) {
        self.shown.push(Shown::Note(note));
    }
}

/// A line the terminal shows of a transfer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Note {
    Started(ZmodemDirection, Option<PathBuf>),
    /// Rewritten in place while the file moves.
    Progress {
        name: String,
        done: u64,
        size: u64,
        rate: u64,
    },
    Done {
        direction: ZmodemDirection,
        name: String,
        size: u64,
        rate: u64,
    },
    /// The other end has a file of that name already, and keeps it.
    Skipped(String),
    /// A file to upload that could not be: the others still go.
    FileFailed {
        name: String,
        error: String,
    },
    Failed(Failure),
    Cancelled,
}

/// Why a transfer stopped.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Failure {
    NoAnswer,
    /// The other end cancelled.
    Peer,
    Corrupt,
    Read {
        path: PathBuf,
        error: String,
    },
    Write {
        path: PathBuf,
        error: String,
    },
    Disconnected,
    Protocol(String),
}

impl Note {
    /// The note as the terminal shows it, `columns` wide: on the line the
    /// cursor is on, which it takes over, and then on the next, except
    /// for progress, which the next note writes over.
    pub(crate) fn render(&self, columns: usize) -> String {
        let line = match self {
            Note::Started(ZmodemDirection::Upload, _) => t!("terminal.zmodem.uploading"),
            Note::Started(ZmodemDirection::Download, folder) => t!(
                "terminal.zmodem.downloading",
                folder = folder
                    .as_deref()
                    .map(|folder| folder.display().to_string())
                    .unwrap_or_default()
            ),
            Note::Progress {
                name,
                done,
                size,
                rate,
            } => {
                let figures = format!(
                    "  {}%  {} / {}  {}/s",
                    percent(*done, *size),
                    format_bytes(*done),
                    format_bytes(*size),
                    format_bytes(*rate)
                );
                let room = columns.saturating_sub(figures.width() + 1);
                return format!("\r\x1b[K{}{figures}", fit(name, room));
            }
            Note::Done {
                direction,
                name,
                size,
                rate,
            } => {
                let (size, rate) = (format_bytes(*size), format!("{}/s", format_bytes(*rate)));
                match direction {
                    ZmodemDirection::Upload => {
                        t!(
                            "terminal.zmodem.uploaded",
                            name = name,
                            size = size,
                            rate = rate
                        )
                    }
                    ZmodemDirection::Download => {
                        t!(
                            "terminal.zmodem.downloaded",
                            name = name,
                            size = size,
                            rate = rate
                        )
                    }
                }
            }
            Note::Skipped(name) => t!("terminal.zmodem.skipped", name = name),
            Note::FileFailed { name, error } => {
                t!("terminal.zmodem.file_failed", name = name, error = error)
            }
            Note::Failed(failure) => match failure {
                Failure::NoAnswer => t!("terminal.zmodem.no_answer"),
                Failure::Peer => t!("terminal.zmodem.peer_cancelled"),
                Failure::Corrupt => t!("terminal.zmodem.corrupt"),
                Failure::Read { path, error } => t!(
                    "terminal.zmodem.read_failed",
                    path = path.display().to_string(),
                    error = error
                ),
                Failure::Write { path, error } => t!(
                    "terminal.zmodem.write_failed",
                    path = path.display().to_string(),
                    error = error
                ),
                Failure::Disconnected => t!("terminal.zmodem.disconnected"),
                Failure::Protocol(error) => t!("terminal.zmodem.protocol", error = error),
            },
            Note::Cancelled => t!("terminal.zmodem.cancelled"),
        };
        format!("\r\x1b[K{line}\r\n")
    }
}

fn percent(done: u64, size: u64) -> u64 {
    if size == 0 {
        100
    } else {
        (done.min(size) as u128 * 100 / size as u128) as u64
    }
}

/// `name`, cut to `room` columns with an ellipsis, so the progress line
/// stays on one line: on two it could not be written over.
fn fit(name: &str, room: usize) -> String {
    if name.width() <= room {
        return name.to_string();
    }
    let mut fitted = String::new();
    let mut width = 0;
    for character in name.chars() {
        let next = character.width().unwrap_or(0);
        if width + next + 1 > room {
            break;
        }
        fitted.push(character);
        width += next;
    }
    fitted.push('…');
    fitted
}

/// Finds where `rz` or `sz` starts in the output, across chunks. The `*`s
/// a header starts with show as they come, which costs nothing: the first
/// note takes over their line. Once a ZDLE follows them, the rest is held
/// back until it is clear whether it is a header.
#[derive(Debug, Default)]
struct Detector {
    /// How much of `HEADER` the output so far ends with.
    matched: usize,
    /// What of it is held back.
    held: Vec<u8>,
}

impl Detector {
    /// `bytes` with what is not a header for the screen, and the header's
    /// direction with everything from it on, once one comes.
    fn scan(&mut self, bytes: &[u8]) -> (Vec<u8>, Option<(ZmodemDirection, Vec<u8>)>) {
        let mut screen = Vec::with_capacity(bytes.len());
        for (index, &byte) in bytes.iter().enumerate() {
            if self.matched == HEADER.len() {
                let direction = match byte {
                    b'0' => Some(ZmodemDirection::Download),
                    b'1' => Some(ZmodemDirection::Upload),
                    _ => None,
                };
                if let Some(direction) = direction {
                    self.matched = 0;
                    self.held.clear();
                    let mut session = HEADER.to_vec();
                    session.extend_from_slice(&bytes[index..]);
                    return (screen, Some((direction, session)));
                }
            } else if byte == HEADER[self.matched] {
                self.matched += 1;
                if self.matched > 2 {
                    self.held.push(byte);
                } else {
                    screen.push(byte);
                }
                continue;
            }
            // Not a header after all: what was held shows.
            screen.append(&mut self.held);
            self.matched = match (byte, self.matched) {
                (b'*', 2) => 2,
                (b'*', _) => 1,
                _ => 0,
            };
            screen.push(byte);
        }
        (screen, None)
    }
}

/// The ZMODEM side of one connection's output: idle, asking the user, or
/// running a transfer.
pub(crate) struct Zmodem {
    encoding: crate::host::TerminalEncoding,
    detector: Detector,
    state: State,
    /// Files dropped on the terminal, for the `rz` typed for them, and
    /// when.
    dropped: Option<(Vec<PathBuf>, Instant)>,
    /// The `OO` a sender ends with, still to keep off the screen.
    over_and_out: u8,
}

enum State {
    Idle,
    /// The user is choosing files or a folder. What the other end sends
    /// meanwhile (its header again) waits for the answer.
    Asking {
        direction: ZmodemDirection,
        held: Vec<u8>,
    },
    Transfer(Box<Transfer>),
    /// A download this side stopped, the sender perhaps still streaming;
    /// see `DRAIN_QUIET`. What came since the last subpacket ended is kept:
    /// what follows it, once quiet, is the shell's prompt.
    Draining {
        since_data: Vec<u8>,
        quiet_at: Instant,
        since: Instant,
    },
}

impl Default for Zmodem {
    fn default() -> Self {
        Self {
            detector: Detector::default(),
            encoding: crate::host::TerminalEncoding::Utf8,
            state: State::Idle,
            dropped: None,
            over_and_out: 0,
        }
    }
}

impl Zmodem {
    pub(super) fn with_encoding(mut self, encoding: crate::host::TerminalEncoding) -> Self {
        self.encoding = encoding;
        self
    }

    /// Whether a transfer has the terminal: running, or waiting for the
    /// user to choose. The keys then go nowhere but Ctrl+C.
    pub(crate) fn active(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    /// When [`Self::tick`] is due, while a transfer runs.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        match &self.state {
            State::Transfer(transfer) => Some(transfer.next_tick),
            State::Draining { quiet_at, .. } => Some(*quiet_at),
            _ => None,
        }
    }

    /// Take a chunk of output. `detect` is false while something else
    /// reads the output for markers (`exec --terminal`): no transfer
    /// starts then.
    pub(crate) fn feed(&mut self, bytes: &[u8], now: Instant, detect: bool) -> Fed {
        let mut fed = Fed::default();
        self.run(bytes.to_vec(), now, detect, &mut fed);
        fed
    }

    fn run(&mut self, mut input: Vec<u8>, now: Instant, detect: bool, fed: &mut Fed) {
        loop {
            match &mut self.state {
                State::Idle => {
                    let os = input
                        .iter()
                        .take(self.over_and_out as usize)
                        .take_while(|byte| **byte == b'O')
                        .count();
                    input.drain(..os);
                    if input.is_empty() {
                        self.over_and_out -= os as u8;
                        return;
                    }
                    self.over_and_out = 0;
                    if !detect {
                        fed.output(&input);
                        return;
                    }
                    let (screen, found) = self.detector.scan(&input);
                    fed.output(&screen);
                    let Some((direction, session)) = found else {
                        return;
                    };
                    let dropped = self
                        .dropped
                        .take()
                        .filter(|(_, at)| now.duration_since(*at) <= DROPPED_FOR);
                    match (direction, dropped) {
                        (ZmodemDirection::Upload, Some((files, _))) => {
                            self.state = Transfer::send(files, now, fed);
                        }
                        _ => {
                            fed.ask = Some(direction);
                            self.state = State::Asking {
                                direction,
                                held: Vec::new(),
                            };
                        }
                    }
                    input = session;
                }
                State::Asking { held, .. } => {
                    // The other end only says its header again meanwhile.
                    if held.len() < HELD_AT_MOST {
                        held.extend_from_slice(&input);
                    }
                    return;
                }
                State::Transfer(transfer) => match transfer.pump(&input, now, fed) {
                    None => return,
                    Some(ended) => {
                        if ended.received {
                            self.over_and_out = 2;
                        }
                        self.state = if ended.drain {
                            State::Draining {
                                since_data: Vec::new(),
                                quiet_at: now + DRAIN_QUIET,
                                since: now,
                            }
                        } else {
                            State::Idle
                        };
                        input = ended.leftover;
                    }
                },
                State::Draining {
                    since_data,
                    quiet_at,
                    since,
                } => {
                    if input.is_empty() {
                        return;
                    }
                    since_data.append(&mut input);
                    if now.duration_since(*since) >= DRAIN_AT_MOST {
                        let shown = std::mem::take(since_data);
                        self.state = State::Idle;
                        input = shown;
                        continue;
                    }
                    if let Some(end) = last_subpacket_end(since_data) {
                        since_data.drain(..end);
                    } else if since_data.len() > DRAIN_KEPT {
                        since_data.clear();
                    }
                    *quiet_at = now + DRAIN_QUIET;
                    return;
                }
            }
        }
    }

    /// Take what the UI thread says.
    pub(crate) fn control(&mut self, control: ZmodemControl, now: Instant) -> Fed {
        let mut fed = Fed::default();
        match control {
            // None: the `rz` for them could not be typed.
            ZmodemControl::Dropped(files) => {
                self.dropped = (!files.is_empty()).then_some((files, now));
            }
            ZmodemControl::Cancel => match &mut self.state {
                State::Idle => {}
                // Enough waiting for the sender: what comes is shown.
                State::Draining { since_data, .. } => {
                    let shown = text_after_data(since_data, self.encoding).to_vec();
                    self.state = State::Idle;
                    self.run(shown, now, true, &mut fed);
                }
                State::Asking { .. } => self.decline(&mut fed),
                State::Transfer(transfer) => {
                    transfer.abort(Note::Cancelled);
                    self.run(Vec::new(), now, true, &mut fed);
                }
            },
            ZmodemControl::Reply(reply) => {
                let State::Asking { direction, held } = &mut self.state else {
                    // An answer for a connection since gone.
                    return fed;
                };
                let (direction, held) = (*direction, std::mem::take(held));
                match (direction, reply) {
                    // `rz` said it is ready: the sender takes that at once.
                    (ZmodemDirection::Upload, ZmodemReply::Files(files)) => {
                        self.state = Transfer::send(files, now, &mut fed);
                        self.run(held, now, true, &mut fed);
                    }
                    // The receiver says it is ready itself. Answering the
                    // `sz` header held meanwhile too would say it twice,
                    // and `sz` would offer its first file twice.
                    (ZmodemDirection::Download, ZmodemReply::Folder(folder)) => {
                        self.state = Transfer::receive(folder, now, &mut fed);
                        self.run(Vec::new(), now, true, &mut fed);
                    }
                    _ => self.decline(&mut fed),
                }
            }
        }
        fed
    }

    /// Stop what the other end started before it began: the user chose
    /// nothing, or pressed Ctrl+C meanwhile.
    fn decline(&mut self, fed: &mut Fed) {
        fed.wire.extend_from_slice(&ABORT);
        fed.note(Note::Cancelled);
        self.state = State::Idle;
    }

    /// Time has passed: see [`Self::deadline`].
    pub(crate) fn tick(&mut self, now: Instant) -> Fed {
        let mut fed = Fed::default();
        match &mut self.state {
            State::Transfer(transfer) if now >= transfer.next_tick => {
                transfer.quiet(now);
                self.run(Vec::new(), now, true, &mut fed);
            }
            // The sender has stopped: what text came last is for the
            // screen.
            State::Draining {
                since_data,
                quiet_at,
                ..
            } if now >= *quiet_at => {
                let shown = text_after_data(since_data, self.encoding).to_vec();
                self.state = State::Idle;
                self.run(shown, now, true, &mut fed);
            }
            _ => {}
        }
        fed
    }

    /// The connection is gone, and with it any transfer.
    pub(crate) fn disconnected(&mut self) -> Fed {
        let mut fed = Fed::default();
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Idle | State::Draining { .. } => {}
            State::Asking { .. } => fed.note(Note::Failed(Failure::Disconnected)),
            State::Transfer(mut transfer) => {
                transfer.peer.discard();
                fed.note(Note::Failed(Failure::Disconnected));
            }
        }
        self.dropped = None;
        fed
    }
}

/// A transfer in progress.
struct Transfer {
    peer: Peer,
    /// CANs in a row at the end of what came so far.
    cans: usize,
    /// Corrupt headers since the last good data.
    errors: u32,
    /// Times in a row the other end said nothing for `QUIET`.
    quiet: u32,
    next_tick: Instant,
    /// The note the transfer ends with, when this side stops it.
    ending: Option<Note>,
}

/// How a transfer ended.
struct Ended {
    /// What came after it, for the screen.
    leftover: Vec<u8>,
    /// Files were received: the sender's `OO` follows.
    received: bool,
    /// This side stopped a download: see `DRAIN_QUIET`.
    drain: bool,
}

enum Peer {
    Receiving {
        receiver: Receiver,
        folder: PathBuf,
        file: Option<Incoming>,
    },
    Sending {
        sender: Sender,
        files: VecDeque<PathBuf>,
        file: Option<Outgoing>,
        /// The next file is to start, or the session to end.
        next: bool,
        pace: Pace,
    },
}

/// The window an upload streams, after how long the last lot of
/// subpackets took: see `WINDOW_FIRST`.
struct Pace {
    window: usize,
    /// When the lot being sent began.
    started: Option<Instant>,
    /// The sender is waiting for the other end: the next file data read
    /// begins a new lot.
    waiting: bool,
}

impl Pace {
    fn new() -> Self {
        Self {
            window: WINDOW_FIRST,
            started: None,
            waiting: false,
        }
    }

    /// File data is read for the sender: when it had been waiting, the
    /// lot before was acknowledged (or asked for again), and how long it
    /// took sets the window.
    fn moved(&mut self, sender: &mut Sender, now: Instant) {
        if !std::mem::take(&mut self.waiting) {
            return;
        }
        if let Some(started) = self.started.replace(now) {
            let took = now.duration_since(started);
            let window = if took < FRAME_QUICK {
                (self.window * 2).min(WINDOW_MOST)
            } else if took > FRAME_SLOW {
                (self.window / 2).max(1)
            } else {
                self.window
            };
            if window != self.window {
                self.window = window;
                sender.set_streaming_window(window);
            }
        }
    }

    /// The sender has nothing to send until the other end answers.
    fn wait(&mut self) {
        self.waiting = true;
    }

    /// A file is done: the time to the next one's data is not the link's.
    fn file_done(&mut self) {
        self.started = None;
        self.waiting = false;
    }
}

/// `bytes` from the sender as they go on the wire, with NULs escaped
/// too: busybox's telnetd drops a NUL that ends what it reads from the
/// connection, wherever in the data that falls. ZMODEM lets any control
/// character go as ZDLE and the character with bit 6 flipped (lrzsz's
/// `sz -e` sends all of them so), and every receiver reads it back.
fn put_escaped(wire: &mut Vec<u8>, bytes: &[u8]) {
    for &byte in bytes {
        if byte == 0 {
            wire.extend_from_slice(&[ZDLE, 0x40]);
        } else {
            wire.push(byte);
        }
    }
}

/// How far a file has come, for the progress line.
struct Progress {
    name: String,
    size: u64,
    done: u64,
    started: Instant,
    shown: Option<Instant>,
}

impl Progress {
    fn new(name: String, size: u64, now: Instant) -> Self {
        Self {
            name,
            size,
            done: 0,
            started: now,
            shown: None,
        }
    }

    /// Bytes a second, so far.
    fn rate(&self, now: Instant) -> u64 {
        let seconds = now.duration_since(self.started).as_secs_f64();
        if seconds < 0.001 {
            self.done
        } else {
            (self.done as f64 / seconds) as u64
        }
    }

    fn show(&mut self, now: Instant, fed: &mut Fed) {
        if self
            .shown
            .is_some_and(|shown| now.duration_since(shown) < PROGRESS_EVERY)
        {
            return;
        }
        self.shown = Some(now);
        fed.note(Note::Progress {
            name: self.name.clone(),
            done: self.done,
            size: self.size,
            rate: self.rate(now),
        });
    }

    fn done(&self, direction: ZmodemDirection, now: Instant) -> Note {
        Note::Done {
            direction,
            name: self.name.clone(),
            size: self.done,
            rate: self.rate(now),
        }
    }
}

struct Incoming {
    progress: Progress,
    path: PathBuf,
    writer: BufWriter<File>,
    modified: Option<u32>,
}

struct Outgoing {
    progress: Progress,
    path: PathBuf,
    file: File,
    position: u64,
}

impl Transfer {
    fn new(peer: Peer, now: Instant) -> Self {
        Self {
            peer,
            cans: 0,
            errors: 0,
            quiet: 0,
            next_tick: now + QUIET,
            ending: None,
        }
    }

    /// Receive what the other end's `sz` sends into `folder`.
    fn receive(folder: PathBuf, now: Instant, fed: &mut Fed) -> State {
        // No buffer limit and overlapped I/O: lrzsz's `sz` streams only
        // then, instead of waiting a round trip for every kilobyte.
        let receiver = match Receiver::with_flow_control(0, true) {
            Ok(receiver) => receiver,
            Err(error) => return failed(Failure::Protocol(error.to_string()), fed),
        };
        fed.note(Note::Started(
            ZmodemDirection::Download,
            Some(folder.clone()),
        ));
        State::Transfer(Box::new(Self::new(
            Peer::Receiving {
                receiver,
                folder,
                file: None,
            },
            now,
        )))
    }

    /// Send `files` to the other end's `rz`.
    fn send(files: Vec<PathBuf>, now: Instant, fed: &mut Fed) -> State {
        let mut sender = match Sender::new() {
            Ok(sender) => sender,
            Err(error) => return failed(Failure::Protocol(error.to_string()), fed),
        };
        sender.set_streaming_window(WINDOW_FIRST);
        fed.note(Note::Started(ZmodemDirection::Upload, None));
        State::Transfer(Box::new(Self::new(
            Peer::Sending {
                sender,
                files: files.into(),
                file: None,
                next: true,
                pace: Pace::new(),
            },
            now,
        )))
    }

    /// The other end said something: the transfer goes on.
    fn heard(&mut self, now: Instant) {
        self.quiet = 0;
        self.next_tick = now + QUIET;
    }

    /// The other end said nothing for `QUIET`: ask again, or give up.
    fn quiet(&mut self, now: Instant) {
        self.quiet += 1;
        self.next_tick = now + QUIET;
        if self.quiet >= QUIET_TIMES {
            self.abort(Note::Failed(Failure::NoAnswer));
            return;
        }
        // Only sends the handshake again while it is not answered yet.
        let _ = match &mut self.peer {
            Peer::Receiving { receiver, .. } => receiver.timeout(),
            Peer::Sending { sender, .. } => sender.timeout(),
        };
    }

    /// Stop, with `ending` as the last line: the other end hears the CANs.
    fn abort(&mut self, ending: Note) {
        if self.ending.is_some() {
            return;
        }
        self.ending = Some(ending);
        match &mut self.peer {
            Peer::Receiving { receiver, .. } => {
                let _ = receiver.abort();
            }
            Peer::Sending { sender, .. } => sender.abort(),
        }
    }

    /// Where a run of CANs reaches `CANCEL_RUN` in `input`, if one does:
    /// the other end cancelled.
    fn cancelled_at(&mut self, input: &[u8]) -> Option<usize> {
        for (index, byte) in input.iter().enumerate() {
            if *byte == CAN {
                self.cans += 1;
                if self.cans >= CANCEL_RUN {
                    let rest = input[index..]
                        .iter()
                        .take_while(|byte| **byte == CAN)
                        .count();
                    return Some(index + rest);
                }
            } else {
                self.cans = 0;
            }
        }
        None
    }

    /// Feed `input` through, doing what the protocol asks, until it waits
    /// for more. Says how the transfer ended, if it did.
    fn pump(&mut self, input: &[u8], now: Instant, fed: &mut Fed) -> Option<Ended> {
        if let Some(end) = self.cancelled_at(input) {
            self.peer.discard();
            fed.note(Note::Failed(Failure::Peer));
            return Some(Ended {
                leftover: input[end..].to_vec(),
                received: false,
                drain: false,
            });
        }
        let mut input = input;
        loop {
            let step = match &mut self.peer {
                Peer::Receiving {
                    receiver,
                    folder,
                    file,
                } => receive_step(receiver, folder, file, now, fed),
                Peer::Sending {
                    sender,
                    files,
                    file,
                    next,
                    pace,
                } => send_step(sender, files, file, next, pace, now, fed),
            };
            match step {
                Step::Moved => {
                    self.errors = 0;
                    self.heard(now);
                }
                Step::Said => self.heard(now),
                Step::Wrote => {}
                Step::Abort(note) => self.abort(note),
                Step::Ended { completed } => {
                    // The last word (ZFIN, or `OO`) goes out after the end.
                    self.peer.flush(fed);
                    let receiving = matches!(self.peer, Peer::Receiving { .. });
                    let drain = !completed && receiving && self.ending.is_some();
                    if !completed {
                        self.peer.discard();
                        fed.note(self.ending.take().unwrap_or(Note::Failed(Failure::Peer)));
                    }
                    // The last header's line ending is not for the screen.
                    let trailer = input
                        .iter()
                        .take(3)
                        .take_while(|byte| matches!(byte, 0x0d | 0x0a | 0x8d | 0x8a | 0x11))
                        .count();
                    return Some(Ended {
                        leftover: input[trailer..].to_vec(),
                        received: completed && receiving,
                        drain,
                    });
                }
                Step::Idle => {
                    if input.is_empty() {
                        return None;
                    }
                    let submitted = match &mut self.peer {
                        Peer::Receiving { receiver, .. } => receiver.submit_wire(input),
                        Peer::Sending { sender, .. } => sender.submit_wire(input),
                    };
                    match submitted {
                        Ok(0) => return None,
                        Ok(consumed) => input = &input[consumed..],
                        // The rest of the chunk goes with a bad header,
                        // which the other end may send again. Anything
                        // else zmodem2 does not recover from: a corrupt
                        // subpacket it does (it asks for it again).
                        Err(error) => {
                            input = &[];
                            self.errors += 1;
                            let header = matches!(
                                error,
                                zmodem2::Error::UnexpectedCrc16
                                    | zmodem2::Error::UnexpectedCrc32
                                    | zmodem2::Error::MalformedHeader
                                    | zmodem2::Error::MalformedEncoding(_)
                                    | zmodem2::Error::MalformedFrame(_)
                            );
                            if !header || self.errors > ERRORS {
                                self.abort(Note::Failed(Failure::Corrupt));
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Where the last subpacket's end (ZDLE and its kind) starts in `bytes`.
fn last_subpacket_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(2)
        .rposition(|pair| pair[0] == CAN && (b'h'..=b'k').contains(&pair[1]))
}

/// The text in what came last of a sender: what follows its last
/// subpacket's end and CRC-32, or all of it when no data came, if it
/// reads as text, or else the text it ends with. The CANs and backspaces
/// lrzsz's `sz` sends as it gives up (`canit()`) come before its shell's
/// prompt and are left out.
fn text_after_data(bytes: &[u8], encoding: crate::host::TerminalEncoding) -> &[u8] {
    let rest = match last_subpacket_end(bytes) {
        None => bytes,
        Some(end) => {
            let mut at = end + 2;
            for _ in 0..4 {
                match bytes.get(at) {
                    Some(&CAN) => at += 2,
                    Some(_) => at += 1,
                    None => return &[],
                }
            }
            bytes.get(at..).unwrap_or_default()
        }
    };
    let rest = without_cancel(rest);
    if looks_like_text(rest, encoding) {
        return rest;
    }
    // A subpacket cut short: `sz` throws away what it has not written out
    // yet as it gives up. The text at the end, perhaps after a few bytes
    // of the data.
    let start = rest
        .iter()
        .rposition(|byte| !is_text_byte(*byte))
        .map_or(0, |at| at + 1);
    let mut tail = without_cancel(&rest[start..]);
    if looks_like_text(tail, encoding) {
        return tail;
    }
    loop {
        match std::str::from_utf8(tail) {
            Ok(_) => return without_cancel(tail),
            Err(error) => match error.error_len() {
                Some(len) => tail = &tail[error.valid_up_to() + len..],
                None => return without_cancel(&tail[..error.valid_up_to()]),
            },
        }
    }
}

/// `bytes` without the CANs and backspaces they start with.
fn without_cancel(bytes: &[u8]) -> &[u8] {
    let cancel = bytes
        .iter()
        .take_while(|byte| matches!(byte, &&CAN | 8))
        .count();
    &bytes[cancel..]
}

fn looks_like_text(bytes: &[u8], encoding: crate::host::TerminalEncoding) -> bool {
    let valid = match encoding {
        crate::host::TerminalEncoding::Utf8 => std::str::from_utf8(bytes).is_ok(),
        crate::host::TerminalEncoding::Gbk => {
            !encoding_rs::GBK.decode_without_bom_handling(bytes).1
        }
        crate::host::TerminalEncoding::Gb18030 => {
            !encoding_rs::GB18030.decode_without_bom_handling(bytes).1
        }
    };
    valid && bytes.iter().all(|byte| is_text_byte(*byte))
}

fn is_text_byte(byte: u8) -> bool {
    byte >= 0x20 || matches!(byte, b'\r' | b'\n' | b'\t' | 0x07 | 0x08 | 0x1b)
}

/// A transfer that never began.
fn failed(failure: Failure, fed: &mut Fed) -> State {
    fed.wire.extend_from_slice(&ABORT);
    fed.note(Note::Failed(failure));
    State::Idle
}

/// What one action of the protocol came to.
enum Step {
    /// File data moved.
    Moved,
    /// The other end said something.
    Said,
    /// This end said something, which is no sign the other end is there.
    Wrote,
    /// Waiting for the other end.
    Idle,
    Abort(Note),
    Ended {
        completed: bool,
    },
}

impl Peer {
    /// Write what is still to go to the other end.
    fn flush(&mut self, fed: &mut Fed) {
        loop {
            let written = match self {
                Peer::Receiving { receiver, .. } => match receiver.poll() {
                    Action::WriteWire(bytes) => {
                        fed.wire.extend_from_slice(bytes);
                        bytes.len()
                    }
                    _ => return,
                },
                Peer::Sending { sender, .. } => match sender.poll() {
                    Action::WriteWire(bytes) => {
                        put_escaped(&mut fed.wire, bytes);
                        bytes.len()
                    }
                    _ => return,
                },
            };
            match self {
                Peer::Receiving { receiver, .. } => receiver.wire_written(written),
                Peer::Sending { sender, .. } => sender.wire_written(written),
            }
        }
    }

    /// Throw away the file half received: an upload's file stays as it is.
    fn discard(&mut self) {
        if let Peer::Receiving { file, .. } = self
            && let Some(incoming) = file.take()
        {
            drop(incoming.writer);
            let _ = std::fs::remove_file(&incoming.path);
        }
    }
}

fn receive_step(
    receiver: &mut Receiver,
    folder: &Path,
    file: &mut Option<Incoming>,
    now: Instant,
    fed: &mut Fed,
) -> Step {
    match receiver.poll() {
        Action::WriteWire(bytes) => {
            fed.wire.extend_from_slice(bytes);
            let written = bytes.len();
            receiver.wire_written(written);
            Step::Wrote
        }
        Action::WriteFile(bytes) => {
            let Some(incoming) = file.as_mut() else {
                return Step::Abort(Note::Failed(Failure::Protocol(
                    zmodem2::Error::InvalidState.to_string(),
                )));
            };
            if let Err(error) = incoming.writer.write_all(bytes) {
                return Step::Abort(Note::Failed(Failure::Write {
                    path: incoming.path.clone(),
                    error: error.to_string(),
                }));
            }
            let written = bytes.len();
            incoming.progress.done += written as u64;
            incoming.progress.show(now, fed);
            match receiver.file_written(written) {
                Ok(()) => Step::Moved,
                Err(error) => Step::Abort(Note::Failed(Failure::Protocol(error.to_string()))),
            }
        }
        Action::Event(Event::FileStarted(info)) => {
            let name = file_name_of(info.name);
            let size = info.size.map_or(0, |size| u64::from(size.get()));
            let modified = info.modified;
            match create_unique(folder, &name) {
                Ok((path, created)) => {
                    let name = path
                        .file_name()
                        .map_or(name, |name| name.to_string_lossy().into_owned());
                    let mut progress = Progress::new(name, size, now);
                    progress.show(now, fed);
                    *file = Some(Incoming {
                        progress,
                        path,
                        writer: BufWriter::with_capacity(64 * 1024, created),
                        modified,
                    });
                    Step::Said
                }
                Err(error) => Step::Abort(Note::Failed(Failure::Write {
                    path: folder.join(&name),
                    error: error.to_string(),
                })),
            }
        }
        Action::Event(Event::FileCompleted) => {
            let Some(incoming) = file.take() else {
                return Step::Said;
            };
            let Incoming {
                progress,
                path,
                writer,
                modified,
            } = incoming;
            match writer.into_inner() {
                Ok(written) => {
                    if let Some(seconds) = modified {
                        let _ = written
                            .set_modified(UNIX_EPOCH + Duration::from_secs(u64::from(seconds)));
                    }
                    fed.note(progress.done(ZmodemDirection::Download, now));
                    Step::Said
                }
                Err(error) => {
                    let _ = std::fs::remove_file(&path);
                    Step::Abort(Note::Failed(Failure::Write {
                        path,
                        error: error.error().to_string(),
                    }))
                }
            }
        }
        Action::Event(Event::SessionCompleted) => Step::Ended { completed: true },
        Action::Event(Event::Aborted) => Step::Ended { completed: false },
        _ => Step::Idle,
    }
}

fn send_step(
    sender: &mut Sender,
    files: &mut VecDeque<PathBuf>,
    file: &mut Option<Outgoing>,
    next: &mut bool,
    pace: &mut Pace,
    now: Instant,
    fed: &mut Fed,
) -> Step {
    match sender.poll() {
        Action::WriteWire(bytes) => {
            put_escaped(&mut fed.wire, bytes);
            let written = bytes.len();
            sender.wire_written(written);
            Step::Wrote
        }
        Action::ReadFile { offset, max_len } => {
            pace.moved(sender, now);
            let Some(outgoing) = file.as_mut() else {
                return Step::Abort(Note::Failed(Failure::Protocol(
                    zmodem2::Error::InvalidState.to_string(),
                )));
            };
            let offset = u64::from(offset.get());
            let wanted = (max_len as u64).min(outgoing.progress.size.saturating_sub(offset));
            let data = match outgoing.read(offset, wanted as usize) {
                Ok(data) if !data.is_empty() => data,
                Ok(_) => {
                    return Step::Abort(Note::Failed(Failure::Read {
                        path: outgoing.path.clone(),
                        error: io::Error::from(io::ErrorKind::UnexpectedEof).to_string(),
                    }));
                }
                Err(error) => {
                    return Step::Abort(Note::Failed(Failure::Read {
                        path: outgoing.path.clone(),
                        error: error.to_string(),
                    }));
                }
            };
            if let Err(error) = sender.submit_file(&data) {
                return Step::Abort(Note::Failed(Failure::Protocol(error.to_string())));
            }
            outgoing.progress.done = offset + data.len() as u64;
            outgoing.progress.show(now, fed);
            Step::Moved
        }
        Action::Event(Event::FileCompleted) => {
            pace.file_done();
            if let Some(outgoing) = file.take() {
                let progress = &outgoing.progress;
                // lrzsz's `rz` keeps a file it has (ZSKIP) unless run as
                // `rz -y`.
                fed.note(if progress.done < progress.size {
                    Note::Skipped(progress.name.clone())
                } else {
                    progress.done(ZmodemDirection::Upload, now)
                });
            }
            *next = true;
            Step::Said
        }
        Action::Event(Event::SessionCompleted) => Step::Ended { completed: true },
        Action::Event(Event::Aborted) => Step::Ended { completed: false },
        Action::Idle if *next => {
            *next = false;
            start_next(sender, files, file, now, fed)
        }
        _ => {
            if file.is_some() {
                pace.wait();
            }
            Step::Idle
        }
    }
}

/// Offer the next file that can be read, or end the session when there
/// is none left.
fn start_next(
    sender: &mut Sender,
    files: &mut VecDeque<PathBuf>,
    file: &mut Option<Outgoing>,
    now: Instant,
    fed: &mut Fed,
) -> Step {
    while let Some(path) = files.pop_front() {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let opened = File::open(&path).and_then(|opened| {
            let metadata = opened.metadata()?;
            Ok((opened, metadata))
        });
        let (opened, metadata) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                fed.note(Note::FileFailed {
                    name,
                    error: error.to_string(),
                });
                continue;
            }
        };
        if !metadata.is_file() {
            continue;
        }
        let Ok(size) = u32::try_from(metadata.len()) else {
            fed.note(Note::FileFailed {
                name,
                error: t!("terminal.zmodem.too_large").to_string(),
            });
            continue;
        };
        let mut info =
            FileInfo::new(name.as_bytes(), Some(Position::new(size))).with_mode(mode_of(&metadata));
        if let Some(modified) = modified_of(&metadata) {
            info = info.with_modified(modified);
        }
        if let Err(error) = sender.start_file(info) {
            fed.note(Note::FileFailed {
                name,
                error: error.to_string(),
            });
            continue;
        }
        let mut progress = Progress::new(name, u64::from(size), now);
        progress.show(now, fed);
        *file = Some(Outgoing {
            progress,
            path,
            file: opened,
            position: 0,
        });
        return Step::Said;
    }
    match sender.finish() {
        Ok(()) => Step::Said,
        Err(error) => Step::Abort(Note::Failed(Failure::Protocol(error.to_string()))),
    }
}

impl Outgoing {
    /// Up to `len` bytes at `offset`; fewer only at the end of the file.
    fn read(&mut self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        if self.position != offset {
            self.file.seek(SeekFrom::Start(offset))?;
            self.position = offset;
        }
        let mut data = vec![0; len];
        let mut filled = 0;
        while filled < len {
            match self.file.read(&mut data[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        data.truncate(filled);
        self.position += filled as u64;
        Ok(data)
    }
}

/// The name a received file is saved under: the last part of what the
/// sender gave, which may be a path, without control characters (nor,
/// on Windows, what its file names cannot have).
fn file_name_of(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let last = text.rsplit(['/', '\\']).next().unwrap_or_default();
    let name: String = last
        .chars()
        .filter(|character| !character.is_control())
        .map(|character| {
            if cfg!(windows) && "<>:\"|?*".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    match name.trim() {
        "" | "." | ".." => "file".to_string(),
        name => name.to_string(),
    }
}

/// A new file in `folder` called `name`, or `name (1)`, `name (2)` … when
/// it is taken: a download never overwrites.
fn create_unique(folder: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    let as_path = Path::new(name);
    let stem = as_path
        .file_stem()
        .map_or(name.to_string(), |stem| stem.to_string_lossy().into_owned());
    let extension = as_path
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .unwrap_or_default();
    for copy in 0u32.. {
        let candidate = if copy == 0 {
            name.to_string()
        } else {
            format!("{stem} ({copy}){extension}")
        };
        let path = folder.join(candidate);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    unreachable!("there is always another number")
}

/// When the file last changed, in seconds since the epoch, if that fits.
fn modified_of(metadata: &Metadata) -> Option<u32> {
    let modified = metadata.modified().ok()?;
    let seconds = modified
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    u32::try_from(seconds)
        .ok()
        .filter(|_| modified > SystemTime::UNIX_EPOCH)
}

/// The mode `rz` gives the file: a regular file's, with this one's
/// permissions where there are any.
#[cfg(unix)]
fn mode_of(metadata: &Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    0o100_000 | (metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_: &Metadata) -> u32 {
    0o100_644
}

#[cfg(test)]
#[path = "zmodem_tests.rs"]
mod tests;
