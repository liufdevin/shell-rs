//! [`Zmodem`] against zmodem2's own sender and receiver standing in for
//! the other end's `sz` and `rz`, with the files in a temporary folder.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use zmodem2::{Action, Event, FileInfo, Position, Receiver, Sender};

use super::*;

/// The other end's `sz`, offering `files`, which ends with `$ `, its
/// shell's prompt, once it is done.
struct Sz {
    sender: Sender,
    files: VecDeque<(String, Vec<u8>)>,
    file: Option<Vec<u8>>,
    next: bool,
    done: bool,
    /// The shell's prompt is out.
    prompted: bool,
    /// Garble a byte of the next large chunk, once.
    garble: Option<Garble>,
}

/// What the line garbles.
#[derive(Clone, Copy)]
enum Garble {
    /// A byte of a file's data, which is sent again.
    Data,
    /// A ZDLE, so a subpacket runs on: the receiver gives up.
    Escape,
}

impl Sz {
    fn new(files: &[(&str, &[u8])]) -> Self {
        let mut sender = Sender::new().unwrap();
        sender.set_streaming_window(16);
        Self {
            sender,
            files: files
                .iter()
                .map(|(name, data)| (name.to_string(), data.to_vec()))
                .collect(),
            file: None,
            next: true,
            done: false,
            prompted: false,
            garble: None,
        }
    }

    /// What `sz` writes first: lrzsz's starts the `rz` of a terminal
    /// without ZMODEM.
    fn start(&mut self) -> Vec<u8> {
        let mut out = b"rz\r".to_vec();
        out.extend(self.pump(&[]));
        out
    }

    fn pump(&mut self, mut input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            match self.sender.poll() {
                Action::WriteWire(bytes) => {
                    out.extend_from_slice(bytes);
                    let written = bytes.len();
                    self.sender.wire_written(written);
                }
                Action::ReadFile { offset, max_len } => {
                    let data = &self.file.as_ref().unwrap()[offset.get() as usize..];
                    let len = max_len.min(data.len());
                    let chunk = data[..len].to_vec();
                    self.sender.submit_file(&chunk).unwrap();
                }
                Action::Event(Event::FileCompleted) => {
                    self.file = None;
                    self.next = true;
                }
                Action::Event(Event::SessionCompleted | Event::Aborted) => self.done = true,
                Action::Idle => {
                    if self.next && !self.done {
                        self.next = false;
                        match self.files.pop_front() {
                            Some((name, data)) => {
                                let size = Position::new(data.len() as u32);
                                self.sender
                                    .start_file(FileInfo::new(name.as_bytes(), Some(size)))
                                    .unwrap();
                                self.file = Some(data);
                            }
                            None => self.sender.finish().unwrap(),
                        }
                        continue;
                    }
                    if input.is_empty() {
                        break;
                    }
                    match self.sender.submit_wire(input) {
                        Ok(0) => break,
                        Ok(consumed) => input = &input[consumed..],
                        Err(_) => input = &[],
                    }
                }
                _ => {}
            }
        }
        if out.len() > 2000
            && let Some(garble) = self.garble.take()
        {
            let at = (out.len() / 2..out.len())
                .find(|at| match garble {
                    Garble::Data => ![out[at - 1], out[*at], out[*at] ^ 1].contains(&CAN),
                    Garble::Escape => out[*at] == CAN,
                })
                .unwrap();
            out[at] ^= 0x01;
        }
        if self.done && !self.prompted {
            self.prompted = true;
            out.extend_from_slice(b"$ ");
        }
        out
    }
}

/// The other end's `rz`, keeping what it receives, and skipping the file
/// called `skip` as lrzsz's skips one it has.
struct Rz {
    receiver: Receiver,
    received: Vec<(String, Vec<u8>)>,
    file: Option<(String, Vec<u8>)>,
    skip: Option<&'static str>,
    done: bool,
}

impl Rz {
    fn new(skip: Option<&'static str>) -> Self {
        let mut receiver = Receiver::with_flow_control(0, true).unwrap();
        receiver.set_manual_file_accept(true);
        Self {
            receiver,
            received: Vec::new(),
            file: None,
            skip,
            done: false,
        }
    }

    fn start(&mut self) -> Vec<u8> {
        let mut out = b"rz waiting to receive.".to_vec();
        out.extend(self.pump(&[]));
        out
    }

    fn pump(&mut self, mut input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            match self.receiver.poll() {
                Action::WriteWire(bytes) => {
                    out.extend_from_slice(bytes);
                    let written = bytes.len();
                    self.receiver.wire_written(written);
                }
                Action::WriteFile(bytes) => {
                    self.file.as_mut().unwrap().1.extend_from_slice(bytes);
                    let written = bytes.len();
                    self.receiver.file_written(written).unwrap();
                }
                Action::Event(Event::FileStarted(info)) => {
                    let name = String::from_utf8_lossy(info.name).into_owned();
                    if self.skip == Some(name.as_str()) {
                        self.receiver.skip_file().unwrap();
                    } else {
                        self.receiver.accept_file_at(0).unwrap();
                        self.file = Some((name, Vec::new()));
                    }
                }
                Action::Event(Event::FileCompleted) => {
                    if let Some(file) = self.file.take() {
                        self.received.push(file);
                    }
                }
                Action::Event(Event::SessionCompleted | Event::Aborted) => self.done = true,
                Action::Idle => {
                    if input.is_empty() {
                        break;
                    }
                    match self.receiver.submit_wire(input) {
                        Ok(0) => break,
                        Ok(consumed) => input = &input[consumed..],
                        Err(_) => input = &[],
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// Either end of the other side.
trait Remote {
    fn pump(&mut self, input: &[u8]) -> Vec<u8>;
}

impl Remote for Sz {
    fn pump(&mut self, input: &[u8]) -> Vec<u8> {
        Sz::pump(self, input)
    }
}

impl Remote for Rz {
    fn pump(&mut self, input: &[u8]) -> Vec<u8> {
        Rz::pump(self, input)
    }
}

/// What a conversation showed.
#[derive(Default)]
struct Talk {
    screen: Vec<u8>,
    notes: Vec<Note>,
    asked: Vec<ZmodemDirection>,
}

impl Talk {
    fn take(&mut self, fed: Fed) -> Vec<u8> {
        self.screen.extend(screen(&fed));
        self.notes.extend(notes(&fed));
        self.asked.extend(fed.ask);
        fed.wire
    }

    fn screen(&self) -> String {
        String::from_utf8_lossy(&self.screen).into_owned()
    }

    fn last(&self) -> Option<&Note> {
        self.notes
            .iter()
            .rfind(|note| !matches!(note, Note::Progress { .. }))
    }
}

/// Let `zmodem` and `remote` talk, starting with what the other end
/// writes `first`, until neither has more to say. The user answers with
/// `reply`.
fn converse(
    zmodem: &mut Zmodem,
    remote: &mut dyn Remote,
    first: Vec<u8>,
    reply: Option<ZmodemReply>,
) -> Talk {
    let mut shown = Talk::default();
    let mut now = Instant::now();
    let mut to_us = first;
    for _ in 0..10_000 {
        now += Duration::from_millis(1);
        let fed = zmodem.feed(&to_us, now, true);
        let ask = fed.ask;
        let mut to_remote = shown.take(fed);
        if ask.is_some()
            && let Some(reply) = reply.clone()
        {
            let fed = zmodem.control(ZmodemControl::Reply(reply), now);
            to_remote.extend(shown.take(fed));
        }
        to_us = remote.pump(&to_remote);
        if to_us.is_empty() && to_remote.is_empty() {
            break;
        }
    }
    shown
}

fn notes(fed: &Fed) -> Vec<Note> {
    fed.shown
        .iter()
        .filter_map(|shown| match shown {
            Shown::Note(note) => Some(note.clone()),
            Shown::Output(_) => None,
        })
        .collect()
}

fn screen(fed: &Fed) -> Vec<u8> {
    fed.shown
        .iter()
        .filter_map(|shown| match shown {
            Shown::Output(output) => Some(output.as_slice()),
            Shown::Note(_) => None,
        })
        .flatten()
        .copied()
        .collect()
}

fn data(len: usize, seed: u8) -> Vec<u8> {
    // Every byte value, ZDLE and IAC included, in an order of its own.
    (0..len)
        .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

#[test]
fn a_header_is_found_whole_or_across_chunks() {
    let mut detector = Detector::default();
    assert_eq!(
        detector.scan(b"plain *text*"),
        (b"plain *text*".to_vec(), None)
    );

    let (screen, found) = detector.scan(b"rz waiting to receive.**\x18B0100000023be50\r\x8a\x11");
    assert_eq!(screen, b"rz waiting to receive.**");
    let (direction, session) = found.unwrap();
    assert_eq!(direction, ZmodemDirection::Upload);
    assert!(session.starts_with(b"**\x18B0100000023be50"));

    // Split after the ZDLE: held back until the rest decides.
    let (screen, found) = detector.scan(b"rz\r**\x18");
    assert_eq!((screen.as_slice(), found), (&b"rz\r**"[..], None));
    let (screen, found) = detector.scan(b"B00000000000000\r\x8a\x11");
    assert!(screen.is_empty());
    let (direction, session) = found.unwrap();
    assert_eq!(direction, ZmodemDirection::Download);
    assert_eq!(session, b"**\x18B00000000000000\r\x8a\x11");

    // Not a header after all: what was held shows.
    let (screen, found) = detector.scan(b"a**\x18B");
    assert_eq!((screen.as_slice(), found), (&b"a**"[..], None));
    let (screen, found) = detector.scan(b"x");
    assert_eq!((screen.as_slice(), found), (&b"\x18Bx"[..], None));

    // A third `*` still starts one; another frame type does not.
    assert!(detector.scan(b"***\x18B00").1.is_some());
    assert!(detector.scan(b"**\x18B02").1.is_none());
}

#[test]
fn sz_files_land_in_the_folder_chosen() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("a.txt"), b"older").unwrap();
    let (first, second) = (data(5000, 1), data(70_000, 7));
    let mut sz = Sz::new(&[
        ("a.txt", &first),
        ("logs/../../etc/b.bin", &second),
        ("empty", b""),
    ]);
    let mut zmodem = Zmodem::default();
    let first_bytes = sz.start();
    let shown = converse(
        &mut zmodem,
        &mut sz,
        first_bytes,
        Some(ZmodemReply::Folder(folder.path().to_path_buf())),
    );

    assert_eq!(shown.asked, [ZmodemDirection::Download]);
    // The name taken gets a number; a path keeps only its last part.
    assert_eq!(
        std::fs::read(folder.path().join("a.txt")).unwrap(),
        b"older"
    );
    assert_eq!(
        std::fs::read(folder.path().join("a (1).txt")).unwrap(),
        first
    );
    assert_eq!(std::fs::read(folder.path().join("b.bin")).unwrap(), second);
    assert_eq!(std::fs::read(folder.path().join("empty")).unwrap(), b"");
    let done: Vec<_> = shown
        .notes
        .iter()
        .filter_map(|note| match note {
            Note::Done { name, size, .. } => Some((name.as_str(), *size)),
            _ => None,
        })
        .collect();
    assert_eq!(done, [("a (1).txt", 5000), ("b.bin", 70_000), ("empty", 0)]);
    assert!(matches!(
        shown.notes.first(),
        Some(Note::Started(ZmodemDirection::Download, Some(_)))
    ));
    // The prompt after it shows; the sender's `OO` does not.
    assert_eq!(shown.screen(), "rz\r**$ ");
    assert!(!zmodem.active());
}

/// Receive `content` from an `sz` whose line garbles a byte.
fn garbled(garble: Garble, content: &[u8]) -> (Talk, tempfile::TempDir) {
    let folder = tempfile::tempdir().unwrap();
    let mut sz = Sz::new(&[("big", content)]);
    sz.garble = Some(garble);
    let mut zmodem = Zmodem::default();
    let first = sz.start();
    let shown = converse(
        &mut zmodem,
        &mut sz,
        first,
        Some(ZmodemReply::Folder(folder.path().to_path_buf())),
    );
    assert!(sz.garble.is_none(), "the line garbled a chunk");
    drained(&mut zmodem);
    (shown, folder)
}

/// Let the time pass that a stopped download waits for the sender to go
/// quiet, and see it is over.
fn drained(zmodem: &mut Zmodem) {
    if let Some(at) = zmodem.deadline() {
        zmodem.tick(at);
    }
    assert!(!zmodem.active());
    assert_eq!(zmodem.deadline(), None);
}

#[test]
fn a_garbled_byte_is_sent_again() {
    let content = data(40_000, 3);
    let (shown, folder) = garbled(Garble::Data, &content);
    assert_eq!(std::fs::read(folder.path().join("big")).unwrap(), content);
    assert!(matches!(shown.last(), Some(Note::Done { .. })));
}

#[test]
fn a_garbled_escape_ends_it_at_once() {
    let (shown, folder) = garbled(Garble::Escape, &data(40_000, 3));
    assert_eq!(shown.last(), Some(&Note::Failed(Failure::Corrupt)));
    assert!(!folder.path().join("big").exists());
}

#[test]
fn rz_takes_the_files_chosen_and_keeps_the_ones_it_has() {
    let folder = tempfile::tempdir().unwrap();
    let (first, second) = (data(3000, 5), data(300_000, 9));
    let paths: Vec<PathBuf> = [("one.txt", &first), ("two.bin", &second), ("kept", &first)]
        .iter()
        .map(|(name, content)| {
            let path = folder.path().join(name);
            std::fs::write(&path, content).unwrap();
            path
        })
        .collect();
    let mut files = paths.clone();
    // A folder and a file that is not there are left out.
    files.insert(1, folder.path().to_path_buf());
    files.push(folder.path().join("gone"));

    let mut rz = Rz::new(Some("kept"));
    let mut zmodem = Zmodem::default();
    let first_bytes = rz.start();
    let shown = converse(
        &mut zmodem,
        &mut rz,
        first_bytes,
        Some(ZmodemReply::Files(files)),
    );

    assert_eq!(shown.asked, [ZmodemDirection::Upload]);
    assert!(rz.done);
    assert_eq!(
        rz.received,
        [
            ("one.txt".to_string(), first),
            ("two.bin".to_string(), second)
        ]
    );
    assert!(shown.notes.contains(&Note::Skipped("kept".into())));
    assert!(
        shown
            .notes
            .iter()
            .any(|note| matches!(note, Note::FileFailed { name, .. } if name == "gone"))
    );
    assert_eq!(shown.screen(), "rz waiting to receive.**");
    assert!(!zmodem.active());
}

/// Upload `content` to `rz` over a link carrying `rate` bytes a second,
/// time passing as the bytes go. Gives what was shown, every byte sent,
/// and the most sent before the other end answered.
fn upload_over(rate: f64, content: &[u8]) -> (Rz, Talk, Vec<u8>, usize) {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("up");
    std::fs::write(&path, content).unwrap();
    let mut rz = Rz::new(None);
    let mut zmodem = Zmodem::default();
    let mut shown = Talk::default();
    let (mut sent, mut most) = (Vec::new(), 0);
    let mut now = Instant::now();
    let mut to_us = rz.start();
    for _ in 0..100_000 {
        let fed = zmodem.feed(&to_us, now, true);
        let ask = fed.ask;
        let mut to_remote = shown.take(fed);
        if ask.is_some() {
            let reply = ZmodemReply::Files(vec![path.clone()]);
            let fed = zmodem.control(ZmodemControl::Reply(reply), now);
            to_remote.extend(shown.take(fed));
        }
        let mut on_the_way =
            Duration::from_secs_f64(to_remote.len() as f64 / rate).max(Duration::from_millis(1));
        while !on_the_way.is_zero() {
            let step = on_the_way.min(Duration::from_millis(500));
            now += step;
            on_the_way -= step;
            let fed = zmodem.tick(now);
            to_remote.extend(shown.take(fed));
        }
        most = most.max(to_remote.len());
        sent.extend_from_slice(&to_remote);
        to_us = rz.pump(&to_remote);
        if to_us.is_empty() && to_remote.is_empty() {
            break;
        }
    }
    (rz, shown, sent, most)
}

#[test]
fn an_upload_sends_no_bare_nul() {
    // busybox's telnetd drops a NUL that ends what it reads.
    let content = data(20_000, 3);
    let (rz, shown, sent, _) = upload_over(1e9, &content);
    assert_eq!(
        rz.received,
        [("up".to_string(), content)],
        "{:?}",
        shown.last()
    );
    assert!(!sent.contains(&0));
}

#[test]
fn the_window_follows_how_fast_the_link_is() {
    // Fast: it grows to the most.
    let content = data(1_200_000, 7);
    let (rz, shown, _, most) = upload_over(1e9, &content);
    assert_eq!(
        rz.received,
        [("up".to_string(), content)],
        "{:?}",
        shown.last()
    );
    assert!(most > 200 * 1024, "{most}");
    // A serial line at 9600 baud: a few kilobytes at a time, each answered
    // within seconds, so the other end is never taken to say nothing.
    let content = data(60_000, 7);
    let (rz, shown, _, most) = upload_over(960.0, &content);
    assert_eq!(
        rz.received,
        [("up".to_string(), content)],
        "{:?}",
        shown.last()
    );
    assert!(most < 5 * 1024, "{most}");
}

#[test]
fn files_dropped_go_to_the_rz_typed_for_them_without_asking() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("dropped.txt");
    std::fs::write(&path, b"dropped").unwrap();
    let mut zmodem = Zmodem::default();
    let now = Instant::now();
    zmodem.control(ZmodemControl::Dropped(vec![path]), now);

    let mut rz = Rz::new(None);
    let first = rz.start();
    let shown = converse(&mut zmodem, &mut rz, first, None);
    assert!(shown.asked.is_empty());
    assert_eq!(
        rz.received,
        [("dropped.txt".to_string(), b"dropped".to_vec())]
    );

    // Files dropped long before are not what a later `rz` is for.
    zmodem.control(
        ZmodemControl::Dropped(vec![folder.path().join("dropped.txt")]),
        now - DROPPED_FOR - Duration::from_secs(1),
    );
    let mut rz = Rz::new(None);
    let first = rz.start();
    let shown = converse(&mut zmodem, &mut rz, first, Some(ZmodemReply::Declined));
    assert_eq!(shown.asked, [ZmodemDirection::Upload]);
}

#[test]
fn declining_cancels_the_other_end() {
    let mut zmodem = Zmodem::default();
    let now = Instant::now();
    let fed = zmodem.feed(&Sz::new(&[("a", b"a")]).start(), now, true);
    assert_eq!(fed.ask, Some(ZmodemDirection::Download));
    assert!(zmodem.active());

    let fed = zmodem.control(ZmodemControl::Reply(ZmodemReply::Declined), now);
    assert_eq!(fed.wire, ABORT);
    assert_eq!(notes(&fed), [Note::Cancelled]);
    assert!(!zmodem.active());
}

/// Start receiving `content` as `name` from an `sz` into `folder`, and
/// stop halfway.
fn halfway(zmodem: &mut Zmodem, folder: &std::path::Path, content: &[u8]) -> (Sz, Instant) {
    let mut sz = Sz::new(&[("half", content)]);
    let now = Instant::now();
    zmodem.feed(&sz.start(), now, true);
    let fed = zmodem.control(
        ZmodemControl::Reply(ZmodemReply::Folder(folder.to_path_buf())),
        now,
    );
    let mut to_remote = fed.wire;
    // A few rounds: the file is started, part of it is written.
    for _ in 0..3 {
        let to_us = sz.pump(&to_remote);
        to_remote = zmodem.feed(&to_us, now, true).wire;
    }
    assert!(folder.join("half").exists());
    (sz, now)
}

#[test]
fn ctrl_c_cancels_and_swallows_what_the_sender_streams_on() {
    let folder = tempfile::tempdir().unwrap();
    let mut zmodem = Zmodem::default();
    let (_, now) = halfway(&mut zmodem, folder.path(), &data(500_000, 2));
    let fed = zmodem.control(ZmodemControl::Cancel, now);
    assert!(fed.wire.starts_with(&ABORT[..10]));
    assert_eq!(notes(&fed).last(), Some(&Note::Cancelled));
    assert!(!folder.path().join("half").exists());

    // lrzsz's `sz` streams on a while: its data is swallowed.
    let streamed = [&[0; 1000][..], b"\x18h\x01\x02\x03\x04", &[0; 100]].concat();
    let fed = zmodem.feed(&streamed, now, true);
    assert!(fed.shown.is_empty() && fed.wire.is_empty());
    // Its last, then the shell's prompt, split between chunks: the CRC (a
    // byte of it escaped) and what came before is data.
    let fed = zmodem.feed(&[&[0; 1000][..], b"\x18i\x18"].concat(), now, true);
    assert!(fed.shown.is_empty());
    let fed = zmodem.feed(b"Q\xffU:STOPPED\r\n$ ", now, true);
    assert!(fed.shown.is_empty());
    assert!(zmodem.active());
    let fed = zmodem.tick(now + DRAIN_QUIET);
    assert_eq!(screen(&fed), b"STOPPED\r\n$ ");
    assert!(fed.wire.is_empty(), "the CANs go once");
    assert!(!zmodem.active());
}

#[test]
fn text_after_the_sender_stops_shows_once_it_is_quiet() {
    let folder = tempfile::tempdir().unwrap();
    let mut zmodem = Zmodem::default();
    let (_, now) = halfway(&mut zmodem, folder.path(), &data(500_000, 9));
    zmodem.control(ZmodemControl::Cancel, now);
    let fed = zmodem.feed(b"$ ", now, true);
    assert!(fed.shown.is_empty());
    assert_eq!(zmodem.deadline(), Some(now + DRAIN_QUIET));
    let fed = zmodem.tick(now + DRAIN_QUIET);
    assert_eq!(screen(&fed), b"$ ");
    assert!(!zmodem.active());
}

#[test]
fn the_cans_sz_gives_up_with_leave_its_prompt_shown() {
    // A subpacket's end and CRC-32, lrzsz's `canit()`, then the shell.
    let mut bytes = b"data\x18i\x41\x18\x6d\x43\x44".to_vec();
    bytes.extend([CAN; 10]);
    bytes.extend([8; 10]);
    bytes.extend(b"STOPPED\r\n# ");
    assert_eq!(
        text_after_data(&bytes, crate::host::TerminalEncoding::Utf8),
        b"STOPPED\r\n# "
    );
    // `sz` threw away the rest of a subpacket as it gave up.
    let mut bytes = b"data\x18i\x41\x42\x43\x44".to_vec();
    bytes.extend([0; 600]);
    bytes.extend([CAN; 10]);
    bytes.extend([8; 10]);
    bytes.extend("已停止\r\n# ".as_bytes());
    assert_eq!(
        text_after_data(&bytes, crate::host::TerminalEncoding::Utf8),
        "已停止\r\n# ".as_bytes()
    );
    for (encoding, charset) in [
        (crate::host::TerminalEncoding::Gbk, encoding_rs::GBK),
        (crate::host::TerminalEncoding::Gb18030, encoding_rs::GB18030),
    ] {
        let (prompt, _, _) = charset.encode("已停止\r\n# ");
        let mut bytes = b"data\x18i\x41\x42\x43\x44".to_vec();
        bytes.extend([CAN; 10]);
        bytes.extend([8; 10]);
        bytes.extend_from_slice(&prompt);
        assert_eq!(text_after_data(&bytes, encoding), prompt.as_ref());
    }
}

#[test]
fn ctrl_c_again_stops_waiting_for_the_sender() {
    let folder = tempfile::tempdir().unwrap();
    let mut zmodem = Zmodem::default();
    let (_, now) = halfway(&mut zmodem, folder.path(), &data(500_000, 8));
    zmodem.control(ZmodemControl::Cancel, now);
    assert!(zmodem.active());
    zmodem.feed(b"$ ", now, true);
    let fed = zmodem.control(ZmodemControl::Cancel, now);
    assert_eq!(screen(&fed), b"$ ");
    assert!(!zmodem.active());
}

#[test]
fn the_other_end_cancelling_ends_it() {
    let folder = tempfile::tempdir().unwrap();
    let mut zmodem = Zmodem::default();
    let (_, now) = halfway(&mut zmodem, folder.path(), &data(500_000, 4));
    let fed = zmodem.feed(&[&ABORT[..], b"$ "].concat(), now, true);
    assert_eq!(notes(&fed), [Note::Failed(Failure::Peer)]);
    // The backspaces after the CANs, then the prompt.
    assert_eq!(screen(&fed), b"\x08\x08\x08\x08\x08\x08\x08\x08\x08\x08$ ");
    assert!(!zmodem.active());
    assert!(!folder.path().join("half").exists());
}

#[test]
fn an_other_end_that_says_nothing_is_given_up_on() {
    let folder = tempfile::tempdir().unwrap();
    let mut zmodem = Zmodem::default();
    let mut now = Instant::now();
    zmodem.feed(b"**\x18B00000000000000\r\x8a\x11", now, true);
    let fed = zmodem.control(
        ZmodemControl::Reply(ZmodemReply::Folder(folder.path().to_path_buf())),
        now,
    );
    assert!(!fed.wire.is_empty(), "ZRINIT");

    let mut handshakes = 0;
    for _ in 0..QUIET_TIMES {
        assert!(zmodem.active());
        now = zmodem.deadline().unwrap();
        let fed = zmodem.tick(now);
        if notes(&fed).is_empty() {
            assert!(fed.wire.starts_with(b"**\x18B01"), "ZRINIT again");
            handshakes += 1;
        } else {
            assert_eq!(notes(&fed), [Note::Failed(Failure::NoAnswer)]);
            assert!(fed.wire.starts_with(&ABORT[..10]));
        }
    }
    assert_eq!(handshakes, QUIET_TIMES - 1);
    drained(&mut zmodem);
}

#[test]
fn a_connection_lost_ends_it_and_the_file_half_received() {
    let folder = tempfile::tempdir().unwrap();
    let mut zmodem = Zmodem::default();
    halfway(&mut zmodem, folder.path(), &data(500_000, 6));
    let fed = zmodem.disconnected();
    assert_eq!(notes(&fed), [Note::Failed(Failure::Disconnected)]);
    assert!(!zmodem.active());
    assert!(!folder.path().join("half").exists());
}

#[test]
fn nothing_starts_while_detection_is_off() {
    let mut zmodem = Zmodem::default();
    let header = b"**\x18B00000000000000\r\x8a\x11";
    let fed = zmodem.feed(header, Instant::now(), false);
    assert_eq!(screen(&fed), header);
    assert_eq!(fed.ask, None);
    assert!(!zmodem.active());
}

#[test]
fn received_names_are_made_safe() {
    assert_eq!(file_name_of(b"report.pdf"), "report.pdf");
    assert_eq!(file_name_of(b"/var/log/syslog"), "syslog");
    assert_eq!(file_name_of(b"..\\..\\boot.ini"), "boot.ini");
    assert_eq!(file_name_of(b"a/.."), "file");
    assert_eq!(file_name_of(b"bell\x07.txt"), "bell.txt");
    assert_eq!(file_name_of("报告.txt".as_bytes()), "报告.txt");
}

#[test]
fn notes_read_as_lines_and_progress_stays_on_one() {
    let progress = Note::Progress {
        name: "backup.tar.gz".into(),
        done: 23 * 1024 * 1024,
        size: 51 * 1024 * 1024,
        rate: 3 * 1024 * 1024,
    };
    assert_eq!(
        progress.render(80),
        "\r\x1b[Kbackup.tar.gz  45%  23.00 MB / 51.00 MB  3.00 MB/s"
    );
    // Too narrow: the name gives way, the figures stay.
    let narrow = progress.render(44);
    assert_eq!(
        narrow,
        "\r\x1b[Kbacku…  45%  23.00 MB / 51.00 MB  3.00 MB/s"
    );
    assert!(narrow.trim_start_matches("\r\x1b[K").width() < 44);

    let done = Note::Done {
        direction: ZmodemDirection::Upload,
        name: "a.txt".into(),
        size: 2048,
        rate: 1024,
    };
    assert_eq!(
        done.render(80),
        "\r\x1b[Ka.txt  100%  2.00 KB  1.00 KB/s  已上传\r\n"
    );
    assert_eq!(
        Note::Started(ZmodemDirection::Upload, None).render(80),
        "\r\x1b[K正在用 ZMODEM 上传，按 Ctrl+C 取消\r\n"
    );
    assert_eq!(Note::Cancelled.render(80), "\r\x1b[K已取消\r\n");
}

#[test]
fn the_lines_show_between_the_output_around_them() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("a");
    std::fs::write(&path, b"a").unwrap();
    let mut zmodem = Zmodem::default();
    let now = Instant::now();
    zmodem.control(ZmodemControl::Dropped(vec![path]), now);

    // The text before the header, then the transfer's first line.
    let fed = zmodem.feed(&Rz::new(None).start(), now, true);
    assert_eq!(
        fed.shown[..2],
        [
            Shown::Output(b"rz waiting to receive.**".to_vec()),
            Shown::Note(Note::Started(ZmodemDirection::Upload, None)),
        ]
    );
    // Its last line, then the prompt after it.
    let fed = zmodem.feed(&[&ABORT[..5], b"$ "].concat(), now, true);
    assert_eq!(
        fed.shown,
        [
            Shown::Note(Note::Failed(Failure::Peer)),
            Shown::Output(b"$ ".to_vec()),
        ]
    );
}

/// Against lrzsz's own `sz` and `rz`, in a container on a real terminal:
/// what zmodem2 standing in for them cannot show. Needs Docker, the
/// `debian:stable-slim` image and the network (lrzsz comes from Debian's
/// packages; Alpine's main ones have none); run with
/// `cargo test --lib terminal::zmodem -- --ignored` (CI: `shells.yml`).
#[cfg(unix)]
mod lrzsz {
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::testing::{Pty, pty};

    /// A container, removed when dropped: stopping its `docker run` leaves
    /// it running.
    struct Container(String);

    impl Container {
        fn named() -> Self {
            Self(format!("shellrs-zmodem-{:08x}", rand::random::<u32>()))
        }
    }

    impl Drop for Container {
        fn drop(&mut self) {
            crate::testing::sh(&format!("docker rm -f {} >/dev/null 2>&1", self.0), None);
        }
    }

    /// A shell in /tmp of a container with lrzsz, waited for until it
    /// shows its prompt.
    fn start() -> (Pty, Container) {
        let container = Container::named();
        let pty = pty(&[
            "docker",
            "run",
            "--rm",
            "--name",
            &container.0,
            "-i",
            "-t",
            "-w",
            "/tmp",
            "debian:stable-slim",
            "sh",
            "-c",
            "apt-get update -qq >/dev/null 2>&1; apt-get install -y -qq lrzsz >/dev/null 2>&1; echo READY; exec sh",
        ]);
        let started = Instant::now();
        let mut seen = String::new();
        while !seen.contains("READY") {
            assert!(started.elapsed() < Duration::from_secs(120), "{seen}");
            if let Ok(bytes) = pty.output.recv_timeout(Duration::from_millis(100)) {
                seen.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
        while pty.output.recv_timeout(Duration::from_millis(500)).is_ok() {}
        (pty, container)
    }

    /// Type `line` and let [`Zmodem`] take the output, as the terminal's
    /// parser thread does, until the transfer is over and the output shows
    /// `until` (which the line spells with quotes in it, so its echo does
    /// not count). The user answers with `reply`; with `cancel`, Ctrl+C once
    /// that much has come.
    fn talk(
        pty: &mut Pty,
        zmodem: &mut Zmodem,
        line: &str,
        mut reply: Option<ZmodemReply>,
        cancel: Option<u64>,
        until: &str,
    ) -> (String, Vec<Note>) {
        pty.write(line.as_bytes());
        let (mut screen, mut notes, mut transferred) = (Vec::new(), Vec::new(), false);
        let mut cancel = cancel;
        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < Duration::from_secs(120),
                "{}\n{notes:?}",
                String::from_utf8_lossy(&screen)
            );
            let now = Instant::now();
            let mut fed = match pty.output.recv_timeout(Duration::from_millis(10)) {
                Ok(bytes) => zmodem.feed(&bytes, now, true),
                Err(_) => zmodem.tick(now),
            };
            loop {
                if !fed.wire.is_empty() {
                    pty.write(&fed.wire);
                }
                for shown in std::mem::take(&mut fed.shown) {
                    match shown {
                        Shown::Output(bytes) => screen.extend(bytes),
                        Shown::Note(note) => notes.push(note),
                    }
                }
                let progressed = notes.iter().any(|note| match note {
                    Note::Progress { done, .. } => cancel.is_some_and(|at| *done >= at),
                    _ => false,
                });
                fed = if fed.ask.is_some() {
                    zmodem.control(ZmodemControl::Reply(reply.take().unwrap()), now)
                } else if progressed {
                    cancel = None;
                    zmodem.control(ZmodemControl::Cancel, now)
                } else {
                    break;
                };
            }
            transferred |= zmodem.active();
            let text = String::from_utf8_lossy(&screen);
            if transferred && !zmodem.active() && text.contains(until) {
                return (text.into_owned(), notes);
            }
        }
    }

    fn sha256(data: &[u8]) -> String {
        Sha256::digest(data)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// The hash `sha256sum` printed for `name`, a prompt before it or not.
    fn printed_hash<'a>(screen: &'a str, name: &str) -> &'a str {
        screen
            .lines()
            .filter(|line| line.trim_end().ends_with(&format!("  {name}")))
            .find_map(|line| {
                line.split_whitespace()
                    .find(|word| word.len() == 64 && word.chars().all(|c| c.is_ascii_hexdigit()))
            })
            .unwrap_or_else(|| panic!("no hash of {name} in {screen}"))
    }

    #[test]
    #[ignore = "needs Docker"]
    fn sz_and_rz_of_lrzsz_send_and_take_files() {
        let (mut pty, _container) = start();
        let mut zmodem = Zmodem::default();
        let folder = tempfile::tempdir().unwrap();

        // Down: a megabyte of every byte value, and a small text, at once.
        let (screen, notes) = talk(
            &mut pty,
            &mut zmodem,
            "head -c 1048576 /dev/urandom > random; echo hello > small; sha256sum random; sz random small; echo SE''NT\r",
            Some(ZmodemReply::Folder(folder.path().to_path_buf())),
            None,
            "SENT",
        );
        let random = std::fs::read(folder.path().join("random")).unwrap();
        assert_eq!(random.len(), 1_048_576, "{notes:?}");
        assert_eq!(sha256(&random), printed_hash(&screen, "random"));
        assert_eq!(
            std::fs::read(folder.path().join("small")).unwrap(),
            b"hello\n"
        );

        // Up: other bytes of the same size, back under other names.
        let up = folder.path().join("up");
        std::fs::write(&up, data(1_048_576, 11)).unwrap();
        let dot = folder.path().join("dot.txt");
        std::fs::write(&dot, b"dot\n").unwrap();
        let (_, notes) = talk(
            &mut pty,
            &mut zmodem,
            "rz; echo TA''KEN\r",
            Some(ZmodemReply::Files(vec![up.clone(), dot])),
            None,
            "TAKEN",
        );
        assert_eq!(
            notes
                .iter()
                .filter(|note| matches!(note, Note::Done { .. }))
                .count(),
            2,
            "{notes:?}"
        );
        let screen = talk_plain(
            &mut pty,
            "sha256sum up; cat dot.txt; echo CH''ECKED\r",
            "CHECKED",
        );
        assert_eq!(printed_hash(&screen, "up"), sha256(&data(1_048_576, 11)));
        assert!(screen.contains("dot\r\n"));

        // `rz` keeps what it has.
        let (_, notes) = talk(
            &mut pty,
            &mut zmodem,
            "rz; echo AG''AIN\r",
            Some(ZmodemReply::Files(vec![up])),
            None,
            "AGAIN",
        );
        assert!(notes.contains(&Note::Skipped("up".into())), "{notes:?}");
    }

    #[test]
    #[ignore = "needs Docker"]
    fn ctrl_c_stops_the_sz_of_lrzsz() {
        let (mut pty, _container) = start();
        let mut zmodem = Zmodem::default();
        let folder = tempfile::tempdir().unwrap();
        let (screen, notes) = talk(
            &mut pty,
            &mut zmodem,
            "head -c 33554432 /dev/zero > big; sz big; echo ST''OPPED\r",
            Some(ZmodemReply::Folder(folder.path().to_path_buf())),
            Some(1_048_576),
            "STOPPED",
        );
        assert!(notes.contains(&Note::Cancelled), "{notes:?}\n{screen}");
        assert!(!folder.path().join("big").exists());
        // The CANs went to `sz`, not to the shell after it, as typed text.
        let after = talk_plain(&mut pty, "echo AF''TER\r", "AFTER");
        assert!(
            !screen.contains("^X") && !after.contains("^X"),
            "{screen}{after}"
        );
    }

    /// busybox's telnetd (Debian's static busybox has it) in a container
    /// with lrzsz, a shell without a login, on a port of this computer.
    struct Telnetd {
        _container: Container,
        port: u16,
    }

    fn telnetd() -> Telnetd {
        let container = Container::named();
        let started = crate::testing::sh(
            &format!(
                "docker run -d --rm --name {} -p 127.0.0.1::2323 debian:stable-slim sh -c \
                 'apt-get update -qq >/dev/null 2>&1; \
                  apt-get install -y -qq lrzsz busybox-static >/dev/null 2>&1; \
                  exec busybox telnetd -F -p 2323 -l /bin/sh'",
                container.0
            ),
            None,
        );
        assert!(
            started.status.success(),
            "{}",
            String::from_utf8_lossy(&started.stderr)
        );
        let mapped = crate::testing::sh(&format!("docker port {} 2323", container.0), None);
        let port = String::from_utf8(mapped.stdout)
            .unwrap()
            .lines()
            .next()
            .and_then(|line| line.rsplit(':').next()?.trim().parse().ok())
            .expect("the mapped port");
        // Docker takes the connection at once; telnetd answers once lrzsz
        // is in and it runs.
        let since = Instant::now();
        loop {
            assert!(since.elapsed() < Duration::from_secs(180), "telnetd starts");
            if let Ok(mut socket) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                use std::io::Read as _;
                socket
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                if socket.read(&mut [0; 16]).is_ok_and(|read| read > 0) {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Telnetd {
            _container: container,
            port,
        }
    }

    #[test]
    #[ignore = "needs Docker"]
    fn over_telnet_files_go_both_ways() {
        use crate::host::{Host, HostDraft, HostId, TelnetLogin, TelnetSettings};
        use crate::secrets::InMemorySecretStore;
        use crate::ssh::SshConnector;
        use crate::telnet::TelnetTransportProvider;
        use crate::terminal::{
            TelnetTerminalTransportProvider as _, TerminalSize, TerminalTransportCommand,
            TerminalTransportEvent,
        };

        let server = telnetd();
        let directory = tempfile::tempdir().unwrap();
        let back = tempfile::tempdir().unwrap();
        let connector = SshConnector::new(
            directory.path().join("known_hosts"),
            std::sync::Arc::new(InMemorySecretStore::default()),
        );
        let login = TelnetLogin::of(
            &Host::new(
                HostId(1),
                HostDraft::telnet("box", "127.0.0.1", server.port, None),
            ),
            &TelnetSettings::default(),
            None,
        );
        let transport = TelnetTransportProvider::new(connector)
            .factory_for(&login)
            .create();
        let (commands, receiver) = std::sync::mpsc::channel();
        let (events, received) = async_channel::unbounded();
        std::thread::spawn(move || transport.run(TerminalSize::DEFAULT, receiver, events));

        let mut zmodem = Zmodem::default();
        let folder = tempfile::tempdir().unwrap();
        // What the terminal's parser thread does, until `until` shows
        // after a transfer, answering with `reply`.
        let mut talk = |line: &str, mut reply: Option<ZmodemReply>, until: &str| {
            commands
                .send(TerminalTransportCommand::Write(line.as_bytes().to_vec()))
                .unwrap();
            let (mut screen, mut notes, mut transferred) = (Vec::new(), Vec::new(), false);
            let since = Instant::now();
            loop {
                assert!(
                    since.elapsed() < Duration::from_secs(60),
                    "{}\n{notes:?}",
                    String::from_utf8_lossy(&screen)
                );
                let now = Instant::now();
                let mut fed = match received.try_recv() {
                    Ok(TerminalTransportEvent::Output(bytes)) => zmodem.feed(&bytes, now, true),
                    Ok(TerminalTransportEvent::Failed(why)) => panic!("{why}"),
                    Ok(_) => continue,
                    Err(_) => {
                        std::thread::sleep(Duration::from_millis(5));
                        zmodem.tick(now)
                    }
                };
                loop {
                    if !fed.wire.is_empty() {
                        commands
                            .send(TerminalTransportCommand::WriteData(std::mem::take(
                                &mut fed.wire,
                            )))
                            .unwrap();
                    }
                    for shown in std::mem::take(&mut fed.shown) {
                        match shown {
                            Shown::Output(bytes) => screen.extend(bytes),
                            Shown::Note(note) => notes.push(note),
                        }
                    }
                    if fed.ask.is_none() {
                        break;
                    }
                    fed = zmodem.control(ZmodemControl::Reply(reply.take().unwrap()), now);
                }
                transferred |= zmodem.active();
                let text = String::from_utf8_lossy(&screen).into_owned();
                if transferred && !zmodem.active() && text.contains(until) {
                    return (text, notes);
                }
            }
        };

        // Down: carriage returns, NULs and IACs in it, which Telnet
        // changes on the way unless both ends keep to its rules.
        let (screen, notes) = talk(
            "cd /tmp; printf 'a\\r\\000b\\r\\nc\\377\\377d\\r' > crs; head -c 300000 /dev/urandom >> crs; sha256sum crs; sz crs; echo DO''WN\r",
            Some(ZmodemReply::Folder(folder.path().to_path_buf())),
            "DOWN",
        );
        let received_file = std::fs::read(folder.path().join("crs"))
            .unwrap_or_else(|error| panic!("{error}: {notes:?}\n{screen}"));
        assert!(
            received_file.starts_with(b"a\r\0b\r\nc\xff\xffd\r"),
            "{notes:?}"
        );
        assert_eq!(sha256(&received_file), printed_hash(&screen, "crs"));

        // Up: every byte value.
        let up = folder.path().join("up");
        std::fs::write(&up, data(300_000, 13)).unwrap();
        let (_, notes) = talk("rz; echo U''P\r", Some(ZmodemReply::Files(vec![up])), "UP");
        assert!(
            notes.iter().any(|note| matches!(note, Note::Done { .. })),
            "{notes:?}"
        );
        // And back down, to see it arrived whole.
        let (screen, _) = talk(
            "sha256sum up; sz up; echo BA''CK\r",
            Some(ZmodemReply::Folder(back.path().to_path_buf())),
            "BACK",
        );
        assert_eq!(printed_hash(&screen, "up"), sha256(&data(300_000, 13)));
        assert_eq!(
            std::fs::read(back.path().join("up")).unwrap(),
            data(300_000, 13)
        );
    }

    /// Type `line` and read the output until it shows `until`: no transfer.
    fn talk_plain(pty: &mut Pty, line: &str, until: &str) -> String {
        pty.write(line.as_bytes());
        let mut screen = String::new();
        let started = Instant::now();
        while !screen.contains(until) {
            assert!(started.elapsed() < Duration::from_secs(30), "{screen}");
            if let Ok(bytes) = pty.output.recv_timeout(Duration::from_millis(50)) {
                screen.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
        screen
    }
}
