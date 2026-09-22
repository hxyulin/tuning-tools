//! End-to-end session run against the mock carrier.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use studio_carriers::mock::MockLink;
use studio_carriers::Link;
use studio_core::frame;
use studio_core::session::{LinkState, SessionOptions};
use studio_core::{ReadItem, Session, SessionCommand, SessionEvent, SessionSink};
use studio_dwarf::VariableType;

#[derive(Default)]
struct Collect {
    frames: Mutex<Vec<Vec<u8>>>,
    events: Mutex<Vec<SessionEvent>>,
}

impl SessionSink for Collect {
    fn frame(&self, bytes: Vec<u8>) -> bool {
        self.frames.lock().unwrap().push(bytes);
        true
    }
    fn event(&self, event: SessionEvent) {
        self.events.lock().unwrap().push(event);
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn states(sink: &Collect) -> Vec<LinkState> {
    sink.events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Status { state, .. } => Some(*state),
            _ => None,
        })
        .collect()
}

#[test]
fn samples_watched_values_into_frames() {
    let mock = MockLink::new();
    mock.poke(0x2000_0000, &123u32.to_le_bytes());
    mock.poke(0x2000_0004, &0.25f32.to_le_bytes());
    let sink = Arc::new(Collect::default());
    let link = mock.clone();
    let session = Session::spawn(
        move || Ok(Box::new(link) as Box<dyn Link>),
        SessionOptions {
            rate_hz: 500.0,
            elf: None,
            rtt_address: None,
        },
        sink.clone(),
    );
    session.send(SessionCommand::SetWatches(vec![
        ReadItem {
            id: 10,
            address: 0x2000_0000,
            scalar: VariableType::U32,
            bit_offset: None,
            bit_size: None,
        },
        ReadItem {
            id: 11,
            address: 0x2000_0004,
            scalar: VariableType::F32,
            bit_offset: None,
            bit_size: None,
        },
    ]));

    wait_for("frames", || sink.frames.lock().unwrap().len() >= 3);
    wait_for("stats", || {
        sink.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, SessionEvent::Stats { stats, .. } if stats.achieved_hz > 0.0))
    });
    drop(session);

    let frames = sink.frames.lock().unwrap();
    let decoded: Vec<_> = frames
        .iter()
        .map(|f| frame::decode(f).expect("valid frame"))
        .collect();
    let first = &decoded[0];
    assert_eq!(
        first.columns.iter().map(|c| c.0).collect::<Vec<_>>(),
        vec![10, 11]
    );
    assert!(first.columns[0].1.iter().all(|&v| v == 123.0));
    assert!(first.columns[1].1.iter().all(|&v| v == 0.25));
    let times: Vec<f64> = decoded.iter().flat_map(|f| f.times.clone()).collect();
    assert!(times.windows(2).all(|w| w[1] > w[0]), "time is monotonic");
    let events = sink.events.lock().unwrap();
    // Both values sit in one region, so one read per tick, plus the core state per stats report
    let reports = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::Stats { .. }))
        .count();
    assert_eq!(mock.read_calls() as usize, times.len() + reports);
    let achieved = events.iter().rev().find_map(|e| match e {
        SessionEvent::Stats { stats, .. } => Some(stats.achieved_hz),
        _ => None,
    });
    let achieved = achieved.unwrap();
    // Throughput depends on the host; deadline arithmetic is tested in schedule.rs.
    assert!(
        achieved.is_finite() && achieved > 0.0,
        "achieved {achieved} Hz at 500 Hz target"
    );
    drop(events);
    assert_eq!(
        states(&sink),
        vec![
            LinkState::Connecting,
            LinkState::Connected,
            LinkState::Disconnected
        ]
    );
}

#[test]
fn failed_connect_reports_failure() {
    let sink = Arc::new(Collect::default());
    let session = Session::spawn(
        || {
            Err(studio_carriers::CarrierError::ProbeNotFound(
                "any probe".into(),
            ))
        },
        SessionOptions {
            rate_hz: 100.0,
            elf: None,
            rtt_address: None,
        },
        sink.clone(),
    );
    wait_for("session end", || session.is_finished());
    assert_eq!(
        states(&sink),
        vec![LinkState::Connecting, LinkState::Failed]
    );
}

#[test]
fn reads_memory_once_on_request() {
    let mock = MockLink::new();
    mock.poke(0x2000_0010, &[1, 0, 0, 0, 7]);
    mock.fail_reads(0x2000_0100, 0x2000_0200);
    let link = mock.clone();
    let session = Session::spawn(
        move || Ok(Box::new(link) as Box<dyn Link>),
        SessionOptions {
            rate_hz: 100.0,
            elf: None,
            rtt_address: None,
        },
        Arc::new(Collect::default()),
    );
    let read = |regions: Vec<(u64, usize)>| {
        let (reply, rx) = std::sync::mpsc::sync_channel(1);
        assert!(session.send(SessionCommand::Read { regions, reply }));
        rx.recv_timeout(Duration::from_secs(5)).unwrap()
    };

    let bytes = read(vec![(0x2000_0010, 2), (0x2000_0014, 1)]).unwrap();
    assert_eq!(bytes, [vec![1, 0], vec![7]]);
    // One bad region fails the whole read
    assert!(read(vec![(0x2000_0010, 2), (0x2000_0100, 4)]).is_err());
}
