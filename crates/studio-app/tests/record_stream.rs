//! Recording and the TCP stream against the mock target: both see every tick
//! whatever the UI sink does, and neither holds up sampling.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use studio_app::{AppEvent, Carrier, ConnectRequest, StreamOptions, StudioApp, WatchRequest};
use studio_carriers::mock::MockLink;
use studio_carriers::{CarrierError, Link};
use studio_core::catalog::ElfImage;
use studio_core::frame;
use studio_core::session::LinkState;
use studio_core::{SessionEvent, SessionSink};
use studio_dwarf::NodeRef;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../studio-dwarf/tests/fixtures/test_arm.elf"
);
/// The fixture's watchable RAM, all in one read region
const GLOBAL_COUNTER: u64 = 0x2000_0004;
const SENSOR_DATA: u64 = 0x2000_0008;
const RAM: (u64, u64) = (0x2000_0000, 0x2000_0010);

/// A UI sink that takes nothing: every frame is refused, after counting its ticks.
#[derive(Default)]
struct Refusing {
    /// Every produced tick as `(t, values by id)`
    ticks: Mutex<Vec<(f64, HashMap<u32, f64>)>>,
    states: Mutex<Vec<LinkState>>,
}

impl SessionSink for Refusing {
    fn frame(&self, bytes: Vec<u8>) -> bool {
        let frame = frame::decode(&bytes).expect("a TTS1 frame");
        let mut ticks = self.ticks.lock().unwrap();
        for (i, &t) in frame.times.iter().enumerate() {
            let values = frame.columns.iter().map(|(id, v)| (*id, v[i])).collect();
            ticks.push((t, values));
        }
        false
    }
    fn event(&self, event: SessionEvent) {
        if let SessionEvent::Status { state, .. } = event {
            self.states.lock().unwrap().push(state);
        }
    }
}

struct Rig {
    app: Arc<StudioApp>,
    mock: Arc<Mutex<Option<MockLink>>>,
    sink: Arc<Refusing>,
    events: Arc<Mutex<Vec<AppEvent>>>,
}

impl Rig {
    fn new() -> Self {
        let mock = Arc::new(Mutex::new(None::<MockLink>));
        let slot = mock.clone();
        let opener = Arc::new(move |_: &_, elf: &Path| {
            let bytes = std::fs::read(elf).map_err(|e| CarrierError::Other(e.to_string()))?;
            let image = ElfImage::parse(&bytes).map_err(CarrierError::Other)?;
            let link = MockLink::new();
            for (address, data) in image.sections() {
                link.poke(address, data);
            }
            *slot.lock().unwrap() = Some(link.clone());
            Ok(Box::new(link) as Box<dyn Link>)
        });
        let app = Arc::new(StudioApp::with_probe_opener(opener).without_rtt());
        app.open_elf(FIXTURE.into()).expect("fixture ELF opens");
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        app.set_event_sink(Arc::new(move |e| seen.lock().unwrap().push(e)));
        Self {
            app,
            mock,
            sink: Arc::new(Refusing::default()),
            events,
        }
    }

    fn connect(&self, rate_hz: f64) {
        self.app
            .connect(
                ConnectRequest {
                    carrier: Carrier::Probe,
                    probe: None,
                    chip: "STM32F407VGTx".into(),
                    speed_khz: None,
                    port: None,
                    rate_hz,
                },
                self.sink.clone(),
            )
            .unwrap();
        wait_for("connected", || {
            self.app.tap().link_state() == LinkState::Connected
        });
    }

    fn mock(&self) -> MockLink {
        self.mock.lock().unwrap().clone().expect("connected")
    }
}

fn watch(id: u32, symbol: &str, unit: Option<&str>) -> WatchRequest {
    WatchRequest {
        id,
        node: Some(NodeRef::root(symbol)),
        unit: unit.map(Into::into),
        ..Default::default()
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Messages by topic, reading as far as the file goes.
fn read_topics(bytes: &[u8]) -> (HashMap<String, Vec<Value>>, bool) {
    let mut topics: HashMap<String, Vec<Value>> = HashMap::new();
    let stream =
        mcap::MessageStream::new_with_options(bytes, mcap::read::Options::IgnoreEndMagic.into())
            .unwrap();
    let mut complete = true;
    for message in stream {
        match message {
            Ok(m) => topics
                .entry(m.channel.topic.clone())
                .or_default()
                .push(serde_json::from_slice(&m.data).unwrap()),
            Err(_) => {
                complete = false;
                break;
            }
        }
    }
    (topics, complete)
}

/// Keep the target busy: count up, and make `sensor_data` NaN every 7th write.
fn animate(mock: MockLink, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut n = 0u32;
        while !stop.load(Ordering::Relaxed) {
            n += 1;
            mock.poke(GLOBAL_COUNTER, &n.to_le_bytes());
            let v = if n.is_multiple_of(7) {
                f32::NAN
            } else {
                n as f32 * 0.5
            };
            mock.poke(SENSOR_DATA, &v.to_le_bytes());
            std::thread::sleep(Duration::from_micros(300));
        }
    })
}

#[test]
fn records_every_tick_while_the_ui_refuses_frames() {
    let rig = Rig::new();
    let dir = tempfile::tempdir().unwrap();
    rig.connect(1000.0);
    rig.app
        .set_watches(vec![
            watch(1, "global_counter", None),
            watch(2, "sensor_data", Some("rad")),
        ])
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let animator = animate(rig.mock(), stop.clone());
    wait_for("samples", || rig.sink.ticks.lock().unwrap().len() > 50);

    let path = dir.path().join("run.mcap");
    let started = rig.app.start_recording(Some(path.clone()), None).unwrap();
    assert!(started.active);
    assert!(
        rig.app.start_recording(None, None).is_err(),
        "one recording at a time"
    );

    std::thread::sleep(Duration::from_millis(700));
    // A read fault shorter than the loss timeout: ticks with no values
    rig.mock().fail_reads(RAM.0, RAM.1);
    std::thread::sleep(Duration::from_millis(150));
    rig.mock().clear_faults();
    wait_for("recorded samples", || {
        rig.app
            .app_state()
            .recording
            .is_some_and(|r| r.ticks >= 300)
    });

    // Disconnect ends the recording after the session's last batch
    rig.app.disconnect();
    stop.store(true, Ordering::Relaxed);
    animator.join().unwrap();

    let produced = rig.sink.ticks.lock().unwrap().clone();
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        mcap::read::footer(&bytes).is_ok(),
        "a clean stop writes the footer"
    );
    let (topics, complete) = read_topics(&bytes);
    assert!(complete);
    let rows = &topics["/watches"];

    // The recording is exactly the ticks produced from its first one on
    let first = rows[0]["t"].as_f64().unwrap();
    let from = produced
        .iter()
        .position(|(t, _)| *t == first)
        .expect("first tick produced");
    let expected = &produced[from..];
    println!(
        "produced {} ticks ({} during the recording), recorded {}",
        produced.len(),
        expected.len(),
        rows.len()
    );
    assert_eq!(rows.len(), expected.len());
    assert!(rows.len() >= 300, "recording contains the awaited samples");
    let mut all_missing = 0;
    let mut nan_sensor = 0;
    for (row, (t, values)) in rows.iter().zip(expected) {
        assert_eq!(row["t"].as_f64().unwrap(), *t);
        for (id, name) in [(1, "global_counter"), (2, "sensor_data")] {
            let v = values[&id];
            match row.get(name) {
                Some(x) => assert_eq!(x.as_f64().unwrap(), v, "{name} at {t}"),
                None => assert!(v.is_nan(), "{name} missing at {t} but read {v}"),
            }
        }
        if row.as_object().unwrap().len() == 1 {
            all_missing += 1;
        } else if row.get("sensor_data").is_none() {
            nan_sensor += 1;
        }
    }
    assert!(
        all_missing > 0,
        "the fault shows as empty ticks: {all_missing}"
    );
    assert!(
        nan_sensor > 0,
        "NaN sensor values are omitted: {nan_sensor}"
    );

    let meta = &topics["/watches/meta"];
    assert_eq!(meta.len(), 1);
    assert_eq!(meta[0]["watches"][1]["name"], "sensor_data");
    assert_eq!(meta[0]["watches"][1]["unit"], "rad");
    assert_eq!(meta[0]["watches"][1]["type"], "volatile float");
    assert!(topics["/session"]
        .iter()
        .any(|m| m["state"] == "disconnected"));

    let last = rig
        .events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find_map(|e| match e {
            AppEvent::Recording(r) => Some(r.clone()),
            _ => None,
        });
    let last = last.unwrap();
    assert!(!last.active);
    assert_eq!(last.ticks, rows.len() as u64);
    assert_eq!(last.dropped, 0);
    assert_eq!(last.bytes, bytes.len() as u64);
    assert!(rig.app.stop_recording().is_err(), "already stopped");

    // CSV: a header with units, one row per tick
    let csv = studio_app::export_csv(&path, None).unwrap();
    assert_eq!(csv.rows, rows.len() as u64);
    assert_eq!(csv.columns, 2);
    assert!(!csv.truncated);
    let text = std::fs::read_to_string(dir.path().join("run.csv")).unwrap();
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("time,global_counter,sensor_data [rad]"));
    assert_eq!(lines.clone().count(), rows.len());
    assert!(
        lines.any(|l| l.ends_with(",,")),
        "empty cells for failed reads"
    );
}

#[test]
fn recording_follows_a_watch_change_and_survives_being_cut_short() {
    let rig = Rig::new();
    let dir = tempfile::tempdir().unwrap();
    // Recording needs a session
    assert!(rig
        .app
        .start_recording(Some(dir.path().join("x.mcap")), None)
        .is_err());
    rig.connect(1000.0);
    rig.app
        .set_watches(vec![watch(1, "global_counter", None)])
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let animator = animate(rig.mock(), stop.clone());
    let state = rig
        .app
        .start_recording(None, Some(dir.path().into()))
        .unwrap();
    let path = PathBuf::from(&state.path);
    assert_eq!(path.parent().unwrap(), dir.path());
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("test_arm-") && name.ends_with(".mcap"),
        "{name}"
    );

    std::thread::sleep(Duration::from_millis(600));
    rig.app
        .set_watches(vec![
            watch(1, "global_counter", None),
            watch(3, "status_flag", Some("flag")),
        ])
        .unwrap();
    // A unit change alone is a new meta, not a new session watch set
    std::thread::sleep(Duration::from_millis(600));
    rig.app
        .set_watches(vec![
            watch(1, "global_counter", Some("count")),
            watch(3, "status_flag", Some("flag")),
        ])
        .unwrap();
    // Past a periodic flush: the file on disk now holds whole chunks
    std::thread::sleep(Duration::from_millis(1100));
    let partial = std::fs::read(&path).unwrap();

    let stopped = rig.app.stop_recording().unwrap();
    assert!(!stopped.active);
    stop.store(true, Ordering::Relaxed);
    animator.join().unwrap();
    rig.app.disconnect();

    let bytes = std::fs::read(&path).unwrap();
    let (topics, complete) = read_topics(&bytes);
    assert!(complete);
    let meta = &topics["/watches/meta"];
    let names = |m: &Value| -> Vec<String> {
        m["watches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| {
                format!(
                    "{}:{}",
                    w["name"].as_str().unwrap(),
                    w["unit"].as_str().unwrap_or("")
                )
            })
            .collect()
    };
    let history: Vec<Vec<String>> = meta.iter().map(names).collect();
    assert_eq!(
        history,
        vec![
            vec!["global_counter:".to_string()],
            vec!["global_counter:".into(), "status_flag:flag".into()],
            vec!["global_counter:count".into(), "status_flag:flag".into()],
        ]
    );
    let rows = &topics["/watches"];
    assert!(rows.first().unwrap().get("status_flag").is_none());
    assert!(rows.last().unwrap().get("status_flag").is_some());
    // Ticks stay contiguous across the change
    let times: Vec<f64> = rows.iter().map(|r| r["t"].as_f64().unwrap()).collect();
    assert!(times.windows(2).all(|w| w[1] > w[0]));
    let csv = studio_app::export_csv(&path, Some(&dir.path().join("out.csv"))).unwrap();
    let text = std::fs::read_to_string(&csv.path).unwrap();
    assert_eq!(
        text.lines().next(),
        Some("time,global_counter [count],status_flag [flag]")
    );
    assert_eq!(csv.rows, rows.len() as u64);
    // Before status_flag was watched its cell is empty
    assert!(text.lines().nth(1).unwrap().ends_with(','));

    // The copy taken mid-recording has no footer, as if the app had been killed
    assert!(mcap::read::footer(&partial).is_err());
    let (cut, complete) = read_topics(&partial);
    assert!(!complete || cut["/watches"].len() < rows.len());
    let cut_rows = cut["/watches"].len();
    println!(
        "cut-short copy: {cut_rows} of {} ticks readable",
        rows.len()
    );
    assert!(cut_rows > 0, "a flushed chunk must be recoverable");
    assert_eq!(
        cut["/watches"],
        rows[..cut_rows],
        "recovered samples are an exact prefix"
    );
    // Chopping mid-chunk loses only that chunk
    let chopped = &bytes[..bytes.len() * 2 / 3];
    let (chop, _) = read_topics(chopped);
    assert!(!chop["/watches"].is_empty());
    let cut_path = dir.path().join("cut.mcap");
    std::fs::write(&cut_path, &partial).unwrap();
    let csv = studio_app::export_csv(&cut_path, None).unwrap();
    assert!(csv.truncated);
    assert_eq!(csv.rows, cut_rows as u64);
}

fn read_lines(stream: TcpStream, out: Arc<Mutex<Vec<Value>>>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            out.lock()
                .unwrap()
                .push(serde_json::from_str(&line).unwrap());
        }
    })
}

#[test]
fn stream_serves_every_batch_and_a_stalled_client_holds_nothing_up() {
    let rig = Rig::new();
    let mut options = StreamOptions::new(0, false);
    options.client_queue = 8;
    options.drop_limit = 20;
    let state = rig.app.start_stream(options).unwrap();
    assert!(state.listening);
    let address = rig.app.stream_address().unwrap();
    assert!(address.ip().is_loopback());

    // A client may connect before any session: it gets a hello only
    let early = Arc::new(Mutex::new(Vec::new()));
    let early_reader = read_lines(TcpStream::connect(address).unwrap(), early.clone());
    wait_for("hello", || !early.lock().unwrap().is_empty());
    assert_eq!(early.lock().unwrap()[0]["type"], "hello");
    assert_eq!(early.lock().unwrap()[0]["session"]["connected"], false);

    rig.connect(1000.0);
    // Long distinct names fill OS socket buffers even on slow CI schedulers.
    let mut watches = vec![
        watch(1, "global_counter", None),
        watch(2, "sensor_data", Some("rad")),
    ];
    for id in 10..70 {
        let mut w = watch(id, "global_counter", None);
        w.name = Some(format!("copy{id}_{}", "x".repeat(256)));
        watches.push(w);
    }
    rig.app.set_watches(watches).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let animator = animate(rig.mock(), stop.clone());

    // The set reaches the stream with the first batch sampled under it
    wait_for("samples of the new set", || {
        early
            .lock()
            .unwrap()
            .iter()
            .any(|m| m["type"] == "samples" && m["values"].as_object().unwrap().len() == 62)
    });
    let stalled = TcpStream::connect(address).unwrap();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let reader = read_lines(TcpStream::connect(address).unwrap(), lines.clone());
    wait_for("clients", || rig.app.app_state().stream.clients == 3);
    let produced_before = rig.sink.ticks.lock().unwrap().len();
    wait_for("stalled client disconnected with dropped batches", || {
        let state = rig.app.app_state().stream;
        state.dropped > 0 && state.clients == 2
    });
    let produced = rig.sink.ticks.lock().unwrap().len() - produced_before;
    assert!(produced > 0, "sampling continued during backpressure");

    rig.app.disconnect();
    std::thread::sleep(Duration::from_millis(300));
    let stream_state = rig.app.app_state().stream;
    stop.store(true, Ordering::Relaxed);
    animator.join().unwrap();
    rig.app.stop_stream();
    reader.join().unwrap();
    early_reader.join().unwrap();
    drop(stalled);

    let lines = lines.lock().unwrap();
    let kinds: Vec<&str> = lines.iter().map(|m| m["type"].as_str().unwrap()).collect();
    assert_eq!(kinds[0], "hello");
    let hello = &lines[0];
    assert_eq!(hello["version"], 1);
    assert_eq!(hello["session"]["elf"], "test_arm.elf");
    assert_eq!(hello["watches"].as_array().unwrap().len(), 62);
    assert_eq!(hello["watches"][1]["unit"], "rad");
    let samples: Vec<&Value> = lines.iter().filter(|m| m["type"] == "samples").collect();
    assert!(!kinds.contains(&"dropped"), "the reading client kept up");
    let seqs: Vec<u64> = samples.iter().map(|m| m["seq"].as_u64().unwrap()).collect();
    assert!(
        seqs.windows(2).all(|w| w[1] == w[0] + 1),
        "no batch skipped"
    );
    let ticks: usize = samples
        .iter()
        .map(|m| m["t"].as_array().unwrap().len())
        .sum();
    for m in &samples {
        let n = m["t"].as_array().unwrap().len();
        let values = m["values"].as_object().unwrap();
        assert_eq!(values.len(), 62);
        assert!(values.values().all(|v| v.as_array().unwrap().len() == n));
    }
    let nulls = samples
        .iter()
        .flat_map(|m| m["values"]["sensor_data"].as_array().unwrap())
        .filter(|v| v.is_null())
        .count();
    println!(
        "reader: {} samples messages, {ticks} ticks, {nulls} null sensor values; \
         produced {produced} ticks during backpressure; stream state {stream_state:?}",
        samples.len()
    );
    // Every tick produced while it was connected reached it
    let all = rig.sink.ticks.lock().unwrap();
    let first = samples[0]["t"][0].as_f64().unwrap();
    let from = all.iter().position(|(t, _)| *t == first).unwrap();
    assert_eq!(ticks, all.len() - from);
    assert!(nulls > 0);
    assert!(kinds.contains(&"status"));

    // The stalled client lost batches or was cut off; the others did not
    assert!(stream_state.dropped > 0, "{stream_state:?}");
    assert!(stream_state.clients <= 2, "{stream_state:?}");
    // The early client stayed through the connect
    let early = early.lock().unwrap();
    assert!(early.iter().any(|m| m["type"] == "samples"));
}
