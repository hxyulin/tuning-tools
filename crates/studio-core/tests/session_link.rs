//! The framed-link session against a fake firmware that speaks the protocol.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use studio_carriers::{ByteStream, Result as CarrierResult};
use studio_core::catalog::Catalog;
use studio_core::link::{spawn_link, LinkOptions};
use studio_core::session::{LinkState, Session};
use studio_core::wire::{self, cmd, Decoder, Reader};
use studio_core::{frame, SessionCommand, SessionEvent, SessionSink};

const KP: u32 = 0x1111_0001;
const LIMIT: u32 = 0x1111_0002;
const TICKS: u32 = 0x1111_0003;

struct Cell {
    id: u32,
    name: &'static str,
    kind: u8,
    access: u8,
    default: u32,
    range: (f32, f32, f32),
    requested: u32,
    applied: u32,
}

struct Firmware {
    cells: Vec<Cell>,
    decoder: Decoder,
    out: VecDeque<u8>,
    lease: Option<u32>,
    watch: Vec<u32>,
    period: Duration,
    next_sample: Instant,
    sample_seq: u16,
    armed: bool,
    saved: Vec<(u32, u32)>,
}

impl Firmware {
    fn new() -> Self {
        let f = |v: f32| v.to_bits();
        let inf = f32::INFINITY;
        Self {
            cells: vec![
                Cell {
                    id: KP,
                    name: "gimbal.pitch.kp",
                    kind: 0,
                    access: 1,
                    default: f(40.0),
                    range: (0.0, 200.0, 0.2),
                    requested: f(40.0),
                    applied: f(40.0),
                },
                Cell {
                    id: LIMIT,
                    name: "gimbal.pitch.limit",
                    kind: 0,
                    access: 2,
                    default: f(1.0),
                    range: (0.0, 5.0, 0.1),
                    requested: f(1.0),
                    applied: f(1.0),
                },
                Cell {
                    id: TICKS,
                    name: "robot.ticks",
                    kind: 2,
                    access: 0,
                    default: 0,
                    range: (-inf, inf, inf),
                    requested: 0,
                    applied: 0,
                },
            ],
            decoder: Decoder::default(),
            out: VecDeque::new(),
            lease: None,
            watch: Vec::new(),
            period: Duration::ZERO,
            next_sample: Instant::now(),
            sample_seq: 0,
            armed: true,
            saved: Vec::new(),
        }
    }

    fn reply(&mut self, code: u8, seq: u16, status: u8, body: &[u8]) {
        let mut payload = vec![status];
        payload.extend_from_slice(body);
        self.out
            .extend(wire::encode(code | wire::REPLY, seq, &payload));
    }

    fn handle(&mut self, code: u8, seq: u16, payload: &[u8]) {
        let mut r = Reader::new(payload);
        match code {
            cmd::HELLO => {
                let mut b = vec![1];
                b.extend_from_slice(&1u32.to_le_bytes());
                b.extend_from_slice(&(self.cells.len() as u16).to_le_bytes());
                b.extend_from_slice(&[0; 4]);
                b.extend_from_slice(&256u16.to_le_bytes());
                b.extend_from_slice(&3000u16.to_le_bytes());
                self.reply(code, seq, 0, &b);
            }
            cmd::LEASE => {
                let token = r.u32().unwrap();
                if self.lease.is_some_and(|t| t != token) {
                    self.reply(code, seq, wire::STATUS_BUSY, &[]);
                } else {
                    self.lease = Some(token);
                    self.reply(code, seq, 0, &[]);
                }
            }
            cmd::RELEASE => self.lease = None,
            cmd::CATALOG => {
                let offset = usize::from(r.u16().unwrap());
                // Two per page, so the session has to page
                let page: Vec<u8> = self
                    .cells
                    .iter()
                    .skip(offset)
                    .take(2)
                    .flat_map(|c| {
                        let mut b = c.id.to_le_bytes().to_vec();
                        b.extend_from_slice(&[c.kind, c.access]);
                        b.extend_from_slice(&c.default.to_le_bytes());
                        for v in [c.range.0, c.range.1, c.range.2] {
                            b.extend_from_slice(&v.to_bits().to_le_bytes());
                        }
                        b.push(c.name.len() as u8);
                        b.extend_from_slice(c.name.as_bytes());
                        b.push(0);
                        b
                    })
                    .collect();
                let returned = self.cells.len().saturating_sub(offset).min(2) as u8;
                let mut b = (self.cells.len() as u16).to_le_bytes().to_vec();
                b.push(returned);
                b.extend_from_slice(&page);
                self.reply(code, seq, 0, &b);
            }
            cmd::READ => {
                let count = r.u8().unwrap();
                let mut b = vec![count];
                for _ in 0..count {
                    let id = r.u32().unwrap();
                    let c = self.cells.iter().find(|c| c.id == id).unwrap();
                    for v in [c.id, c.requested, c.applied] {
                        b.extend_from_slice(&v.to_le_bytes());
                    }
                }
                self.reply(code, seq, 0, &b);
            }
            cmd::WRITE => {
                let token = r.u32().unwrap();
                let id = r.u32().unwrap();
                let slot = r.bytes(8).unwrap();
                let bits = u32::from_le_bytes(slot[4..].try_into().unwrap());
                let status = if self.lease != Some(token) {
                    8
                } else {
                    let armed = self.armed;
                    let c = self.cells.iter_mut().find(|c| c.id == id).unwrap();
                    let v = f32::from_bits(bits);
                    if c.access == 2 && armed {
                        6
                    } else if !(c.range.0..=c.range.1).contains(&v) {
                        5
                    } else {
                        c.requested = bits;
                        c.applied = bits;
                        0
                    }
                };
                self.reply(code, seq, status, &id.to_le_bytes());
            }
            cmd::DISCARD => {
                for c in self.cells.iter_mut().filter(|c| c.access != 0) {
                    c.requested = c.default;
                }
                self.reply(code, seq, 0, &2u16.to_le_bytes());
            }
            cmd::SAVE => {
                self.saved = self.cells.iter().map(|c| (c.id, c.requested)).collect();
                self.reply(code, seq, 0, &1u32.to_le_bytes());
            }
            cmd::WATCH => {
                let period = r.u16().unwrap();
                let count = r.u8().unwrap();
                self.watch = (0..count).map(|_| r.u32().unwrap()).collect();
                self.period = Duration::from_millis(u64::from(period));
                self.next_sample = Instant::now();
                self.reply(code, seq, 0, &[count]);
            }
            cmd::STATS => self.reply(code, seq, 0, &[0; 12]),
            _ => self.reply(code, seq, 2, &[]),
        }
    }

    fn produce_samples(&mut self) {
        if self.watch.is_empty() || self.period.is_zero() {
            return;
        }
        while Instant::now() >= self.next_sample {
            self.next_sample += self.period;
            let ticks = self.cells.iter_mut().find(|c| c.id == TICKS).unwrap();
            ticks.applied += 1;
            let mut b = 123_000u64.to_le_bytes().to_vec();
            b.push(self.watch.len() as u8);
            for id in &self.watch {
                let c = self.cells.iter().find(|c| c.id == *id).unwrap();
                b.extend_from_slice(&c.applied.to_le_bytes());
            }
            self.out
                .extend(wire::encode(cmd::SAMPLE, self.sample_seq, &b));
            self.sample_seq = self.sample_seq.wrapping_add(1);
        }
    }
}

#[derive(Clone)]
struct FakePort(Arc<Mutex<Firmware>>);

impl ByteStream for FakePort {
    fn read(&mut self, buf: &mut [u8]) -> CarrierResult<usize> {
        let n = {
            let mut fw = self.0.lock().unwrap();
            fw.produce_samples();
            let n = buf.len().min(fw.out.len());
            for (slot, byte) in buf.iter_mut().zip(fw.out.drain(..n)) {
                *slot = byte;
            }
            n
        };
        if n == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(n)
    }

    fn write_all(&mut self, bytes: &[u8]) -> CarrierResult<()> {
        let mut fw = self.0.lock().unwrap();
        let mut frames = Vec::new();
        fw.decoder.feed(bytes, |f| frames.push(f));
        for f in frames {
            fw.handle(f.cmd, f.seq, &f.payload);
        }
        Ok(())
    }
}

#[derive(Default)]
struct Collect {
    events: Mutex<Vec<SessionEvent>>,
    samples: Mutex<Vec<(u32, f64)>>,
}

impl SessionSink for Collect {
    fn frame(&self, bytes: Vec<u8>) -> bool {
        let f = frame::decode(&bytes).unwrap();
        let mut s = self.samples.lock().unwrap();
        for (id, values) in f.columns {
            s.extend(values.iter().map(|v| (id, *v)));
        }
        true
    }

    fn event(&self, event: SessionEvent) {
        self.events.lock().unwrap().push(event);
    }
}

impl Collect {
    fn catalog(&self) -> Option<Catalog> {
        self.events.lock().unwrap().iter().find_map(|e| match e {
            SessionEvent::Catalog { catalog } => Some(catalog.clone()),
            _ => None,
        })
    }

    fn state(&self) -> Option<LinkState> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|e| match e {
                SessionEvent::Status { state, .. } => Some(*state),
                _ => None,
            })
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn start(fw: &Arc<Mutex<Firmware>>) -> (Session, Arc<Collect>) {
    let sink = Arc::new(Collect::default());
    let port = FakePort(fw.clone());
    let session = spawn_link(
        move || Ok(Box::new(port) as Box<dyn ByteStream>),
        LinkOptions { rate_hz: 200.0 },
        sink.clone(),
    );
    wait_for("the catalog", || sink.catalog().is_some());
    (session, sink)
}

fn request(session: &Session, id: u32, value: f64) -> Result<(), String> {
    let (reply, rx) = mpsc::sync_channel(1);
    session.send(SessionCommand::Request { id, value, reply });
    rx.recv_timeout(Duration::from_secs(5)).unwrap()
}

#[test]
fn the_catalog_arrives_over_several_pages() {
    let fw = Arc::new(Mutex::new(Firmware::new()));
    let (_session, sink) = start(&fw);
    let catalog = sink.catalog().unwrap();
    let names: Vec<_> = catalog.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        ["gimbal.pitch.kp", "gimbal.pitch.limit", "robot.ticks"]
    );
    assert_eq!(catalog.entries[0].max_step, Some(0.2));
    assert_eq!(catalog.entries[2].min, None, "a watch has no range");
    assert_eq!(sink.state(), Some(LinkState::Connected));
    assert!(fw.lock().unwrap().lease.is_some());
}

#[test]
fn writes_go_through_the_firmware_checks() {
    let fw = Arc::new(Mutex::new(Firmware::new()));
    let (session, _sink) = start(&fw);
    request(&session, KP, 55.5).unwrap();
    assert_eq!(fw.lock().unwrap().cells[0].requested, 55.5f32.to_bits());

    let err = request(&session, LIMIT, 2.0).unwrap_err();
    assert!(err.contains("safe state"), "{err}");
    let err = request(&session, KP, 500.0).unwrap_err();
    assert!(err.contains("0 to 200"), "checked on the host first: {err}");
    let err = request(&session, TICKS, 1.0).unwrap_err();
    assert!(err.contains("read-only"), "{err}");

    let (reply, rx) = mpsc::sync_channel(1);
    session.send(SessionCommand::Discard { reply });
    rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(fw.lock().unwrap().cells[0].requested, 40.0f32.to_bits());

    let (reply, rx) = mpsc::sync_channel(1);
    session.send(SessionCommand::Save { reply });
    rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert!(fw.lock().unwrap().saved.contains(&(KP, 40.0f32.to_bits())));
}

#[test]
fn watched_values_arrive_as_sample_frames() {
    let fw = Arc::new(Mutex::new(Firmware::new()));
    let (session, sink) = start(&fw);
    session.send(SessionCommand::SetCellWatches(vec![(7, TICKS), (8, KP)]));
    wait_for("samples", || sink.samples.lock().unwrap().len() >= 20);
    let samples = sink.samples.lock().unwrap().clone();
    assert!(samples.iter().any(|&(id, v)| id == 7 && v > 0.0));
    assert!(samples.iter().any(|&(id, v)| id == 8 && v == 40.0));
    assert_eq!(fw.lock().unwrap().period, Duration::from_millis(5));

    wait_for("tune values", || {
        sink.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, SessionEvent::Tune { values, .. } if values.len() == 3))
    });
}

#[test]
fn stopping_releases_the_lease_and_the_subscription() {
    let fw = Arc::new(Mutex::new(Firmware::new()));
    let (session, sink) = start(&fw);
    session.send(SessionCommand::SetCellWatches(vec![(1, TICKS)]));
    wait_for("samples", || !sink.samples.lock().unwrap().is_empty());
    drop(session);
    let fw = fw.lock().unwrap();
    assert_eq!(fw.lease, None);
    assert!(fw.watch.is_empty());
    assert_eq!(sink.state(), Some(LinkState::Disconnected));
}

#[test]
fn a_lease_held_by_another_tool_fails_the_connection() {
    let fw = Arc::new(Mutex::new(Firmware::new()));
    fw.lock().unwrap().lease = Some(0xdead);
    let sink = Arc::new(Collect::default());
    let port = FakePort(fw.clone());
    let _session = spawn_link(
        move || Ok(Box::new(port) as Box<dyn ByteStream>),
        LinkOptions { rate_hz: 100.0 },
        sink.clone(),
    );
    wait_for("failure", || sink.state() == Some(LinkState::Failed));
    let message = sink
        .events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find_map(|e| match e {
            SessionEvent::Status { message, .. } => message.clone(),
            _ => None,
        });
    assert!(message.unwrap().contains("another tool"));
}
