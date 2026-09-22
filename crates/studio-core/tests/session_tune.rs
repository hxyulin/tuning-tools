//! Tuning through a session: the mock target holds the fixture firmware's image.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use studio_carriers::mock::MockLink;
use studio_carriers::Link;
use studio_core::catalog::{Catalog, ElfImage, TableLayout};
use studio_core::session::{RequestReply, SessionOptions};
use studio_core::tune::CatalogCheck;
use studio_core::wire::{self, cmd, Decoder};
use studio_core::{Session, SessionCommand, SessionEvent, SessionSink};
use studio_dwarf::ElfParser;

const ELF: &[u8] = include_bytes!("fixtures/rm_telemetry.elf");

#[derive(Default)]
struct Collect(Mutex<Vec<SessionEvent>>);

impl SessionSink for Collect {
    fn frame(&self, _: Vec<u8>) -> bool {
        true
    }
    fn event(&self, event: SessionEvent) {
        self.0.lock().unwrap().push(event);
    }
}

impl Collect {
    fn last_tune(&self) -> Option<SessionEvent> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|e| matches!(e, SessionEvent::Tune { .. }))
            .cloned()
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn fixture() -> (TableLayout, Catalog, MockLink) {
    let elf = ElfParser::parse_bytes(ELF, "rm_telemetry.elf").unwrap();
    let layout = TableLayout::find(&elf).unwrap().unwrap();
    let image = ElfImage::parse(ELF).unwrap();
    let mock = MockLink::new();
    for (at, data) in image.sections() {
        mock.poke(at, data);
    }
    let mut image = image;
    let catalog = layout.read(&mut image).unwrap();
    (layout, catalog, mock)
}

fn start(mock: &MockLink, layout: TableLayout, catalog: Catalog) -> (Session, Arc<Collect>) {
    start_with(mock, layout, catalog, None)
}

fn start_with(
    mock: &MockLink,
    layout: TableLayout,
    catalog: Catalog,
    rtt_address: Option<u64>,
) -> (Session, Arc<Collect>) {
    let sink = Arc::new(Collect::default());
    let link = mock.clone();
    let session = Session::spawn(
        move || Ok(Box::new(link) as Box<dyn Link>),
        SessionOptions {
            rate_hz: 100.0,
            elf: None,
            rtt_address,
        },
        sink.clone(),
    );
    session.send(SessionCommand::SetCatalog(Some(Box::new((
        layout, catalog,
    )))));
    (session, sink)
}

fn request(session: &Session, id: u32, value: f64) -> Result<(), String> {
    call(session, |reply| SessionCommand::Request {
        id,
        value,
        reply,
    })
}

fn call(
    session: &Session,
    command: impl FnOnce(RequestReply) -> SessionCommand,
) -> Result<(), String> {
    let (reply, rx) = mpsc::sync_channel(1);
    assert!(session.send(command(reply)));
    rx.recv_timeout(Duration::from_secs(5)).expect("a reply")
}

const RTT_BLOCK: u64 = 0x3100_0000;

/// Answers frames on the mock's RTT control channel with the status `answer`
/// gives each command and payload; returns every request it saw.
fn fake_firmware(
    mock: &MockLink,
    answer: impl Fn(u8, &[u8]) -> u8 + Send + 'static,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<(u8, Vec<u8>)>> {
    let mock = mock.clone();
    std::thread::spawn(move || {
        let mut decoder = Decoder::default();
        let mut seen = Vec::new();
        loop {
            // Read once more after the stop, for what the session sent as it closed
            let stopping = stop.load(Ordering::Relaxed);
            let mut frames = Vec::new();
            decoder.feed(&mock.take_down(0), |f| frames.push(f));
            for f in frames {
                let status = answer(f.cmd, &f.payload);
                mock.push_up(1, &wire::encode(f.cmd | wire::REPLY, f.seq, &[status]));
                seen.push((f.cmd, f.payload));
            }
            if stopping {
                return seen;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })
}

#[test]
fn requests_land_in_the_cell_and_show_in_the_values() {
    let (layout, catalog, mock) = fixture();
    let kp = catalog.entries[0].clone();
    let (session, sink) = start(&mock, layout, catalog);

    wait_for("the table check", || {
        matches!(
            sink.last_tune(),
            Some(SessionEvent::Tune {
                check: CatalogCheck::Matches,
                ..
            })
        )
    });
    let Some(SessionEvent::Tune { values, .. }) = sink.last_tune() else {
        unreachable!()
    };
    assert_eq!(values.len(), 6);
    assert_eq!((values[0].id, values[0].requested), (kp.id, Some(40.0)));

    request(&session, kp.id, 55.5).unwrap();
    assert_eq!(mock.peek(kp.requested_address, 4), 55.5f32.to_le_bytes());
    wait_for(
        "the request in the values",
        || matches!(sink.last_tune(), Some(SessionEvent::Tune { values, .. }) if values[0].requested == Some(55.5)),
    );

    let err = request(&session, kp.id, 500.0).unwrap_err();
    assert!(err.contains("0 to 200"), "{err}");
    let err = request(&session, 0xdead_beef, 1.0).unwrap_err();
    assert!(err.contains("no value"), "{err}");
    let err = request(&session, values[2].id, 1.0).unwrap_err();
    assert!(err.contains("read-only"), "{err}");
    assert_eq!(mock.peek(kp.requested_address, 4), 55.5f32.to_le_bytes());
}

#[test]
fn a_target_running_another_build_refuses_writes() {
    let (layout, catalog, mock) = fixture();
    let kp = catalog.entries[0].clone();
    // The target's first entry has another name, as a different build would
    let first_name = catalog.entries[0].name.clone();
    let image = ElfImage::parse(ELF).unwrap();
    let at = image
        .sections()
        .find_map(|(at, data)| {
            data.windows(first_name.len())
                .position(|w| w == first_name.as_bytes())
                .map(|i| at + i as u64)
        })
        .expect("name bytes in the image");
    mock.poke(at, b"X");

    let (session, sink) = start(&mock, layout, catalog);
    wait_for("the table check", || {
        matches!(
            sink.last_tune(),
            Some(SessionEvent::Tune {
                check: CatalogCheck::Differs { .. },
                ..
            })
        )
    });
    assert!(
        matches!(sink.last_tune(), Some(SessionEvent::Tune { values, .. }) if values.is_empty()),
        "no values from another build's cells"
    );
    let err = request(&session, kp.id, 50.0).unwrap_err();
    assert!(err.contains("not running the open ELF"), "{err}");
    assert_eq!(mock.peek(kp.requested_address, 4), 40.0f32.to_le_bytes());
}

#[test]
fn a_firmware_with_rtt_control_channels_gets_framed_requests_and_can_save() {
    let (layout, catalog, mock) = fixture();
    let kp = catalog.entries[0].clone();
    let other = catalog.entries[1].clone();
    mock.init_rtt_channels(
        RTT_BLOCK,
        &[("defmt", 256), ("telemetry", 256)],
        &[("control", 256)],
    );
    let stop = Arc::new(AtomicBool::new(false));
    let refused = other.id.to_le_bytes();
    let answer = move |code, payload: &[u8]| {
        let refused = code == cmd::WRITE && payload[4..8] == refused;
        if refused {
            5
        } else {
            0
        }
    };
    let firmware = fake_firmware(&mock, answer, stop.clone());
    let (session, _sink) = start_with(&mock, layout, catalog, Some(RTT_BLOCK));

    request(&session, kp.id, 55.5).unwrap();
    // The firmware applies it, not the session
    assert_eq!(mock.peek(kp.requested_address, 4), 40.0f32.to_le_bytes());
    let err = request(&session, other.id, 1.0).unwrap_err();
    assert!(err.contains("outside the range"), "{err}");
    call(&session, |reply| SessionCommand::Discard { reply }).unwrap();
    call(&session, |reply| SessionCommand::Save { reply }).unwrap();
    drop(session);
    stop.store(true, Ordering::Relaxed);
    let seen = firmware.join().unwrap();

    let commands: Vec<u8> = seen.iter().map(|(c, _)| *c).collect();
    assert_eq!(commands[0], cmd::LEASE, "{commands:?}");
    assert_eq!(commands.last(), Some(&cmd::RELEASE), "{commands:?}");
    let token = &seen[0].1[..4];
    let write = &seen.iter().find(|(c, _)| *c == cmd::WRITE).unwrap().1;
    assert_eq!(&write[..4], token);
    assert_eq!(&write[4..8], &kp.id.to_le_bytes());
    assert_eq!(&write[8..], &wire::slot(kp.kind.tag(), 55.5f32.to_bits()));
    for code in [cmd::DISCARD, cmd::SAVE] {
        let (_, payload) = seen.iter().find(|(c, _)| *c == code).unwrap();
        assert_eq!(&payload[..], token);
    }
}

#[test]
fn a_request_behind_a_refused_lease_says_another_tool_holds_it() {
    let (layout, catalog, mock) = fixture();
    let kp = catalog.entries[0].clone();
    mock.init_rtt_channels(
        RTT_BLOCK,
        &[("defmt", 256), ("telemetry", 256)],
        &[("control", 256)],
    );
    let stop = Arc::new(AtomicBool::new(false));
    // Another tool holds the lease: LEASE is refused and WRITE finds no lease
    let answer = |code, _: &[u8]| if code == cmd::LEASE { 9 } else { 8 };
    let firmware = fake_firmware(&mock, answer, stop.clone());
    let (session, _sink) = start_with(&mock, layout, catalog, Some(RTT_BLOCK));

    let err = request(&session, kp.id, 55.5).unwrap_err();
    assert!(err.contains("another tool holds"), "{err}");
    // Asked again with the next request
    let _ = request(&session, kp.id, 55.5);
    drop(session);
    stop.store(true, Ordering::Relaxed);
    let seen = firmware.join().unwrap();
    let leases = seen.iter().filter(|(c, _)| *c == cmd::LEASE).count();
    assert_eq!(leases, 2, "{seen:?}");
    assert!(seen.iter().all(|(c, _)| *c != cmd::RELEASE), "{seen:?}");
}

#[test]
fn a_firmware_without_rtt_control_channels_is_tuned_through_memory_and_cannot_save() {
    let (layout, catalog, mock) = fixture();
    let kp = catalog.entries[0].clone();
    mock.init_rtt(RTT_BLOCK, "defmt", 256);
    let (session, sink) = start_with(&mock, layout, catalog, Some(RTT_BLOCK));
    wait_for("the table check", || {
        matches!(
            sink.last_tune(),
            Some(SessionEvent::Tune {
                check: CatalogCheck::Matches,
                ..
            })
        )
    });
    request(&session, kp.id, 55.5).unwrap();
    assert_eq!(mock.peek(kp.requested_address, 4), 55.5f32.to_le_bytes());
    let err = call(&session, |reply| SessionCommand::Save { reply }).unwrap_err();
    assert!(err.contains("connect over USB"), "{err}");
}

#[test]
fn rtt_distinguishes_unsupported_save_from_legacy_storage_failure() {
    for status in [11, 12] {
        let (layout, catalog, mock) = fixture();
        let kp = catalog.entries[0].clone();
        mock.init_rtt_channels(
            RTT_BLOCK,
            &[("defmt", 256), ("telemetry", 256)],
            &[("control", 256)],
        );
        let stop = Arc::new(AtomicBool::new(false));
        let firmware = fake_firmware(
            &mock,
            move |code, _| if code == cmd::SAVE { status } else { 0 },
            stop.clone(),
        );
        let (session, _) = start_with(&mock, layout, catalog, Some(RTT_BLOCK));
        let error = call(&session, |reply| SessionCommand::Save { reply }).unwrap_err();
        assert_eq!(error, wire::status_message(status));
        request(&session, kp.id, 55.5).unwrap();
        drop(session);
        stop.store(true, Ordering::Relaxed);
        firmware.join().unwrap();
    }
}
