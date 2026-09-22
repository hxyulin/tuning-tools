//! Drive studio-server over stdio against the mock target: open an ELF, watch
//! values, receive sample frames, disconnect and shut down.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../studio-dwarf/tests/fixtures/test_arm.elf"
);
const TTS1: u32 = u32::from_le_bytes(*b"TTS1");

enum Message {
    Json(Value),
    Frame { session: u32, tts1: Vec<u8> },
}

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Message>,
    next_id: u64,
    /// Messages read while waiting for something else
    backlog: Vec<Message>,
}

fn read_all(mut stdout: ChildStdout, tx: mpsc::Sender<Message>) {
    loop {
        let mut len = [0u8; 4];
        if stdout.read_exact(&mut len).is_err() {
            return;
        }
        let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
        stdout.read_exact(&mut body).expect("whole message");
        let message = match body[0] {
            b'J' => Message::Json(serde_json::from_slice(&body[1..]).expect("JSON message")),
            b'F' => Message::Frame {
                session: u32::from_le_bytes(body[1..5].try_into().unwrap()),
                tts1: body[9..].to_vec(),
            },
            k => panic!("unknown message kind {k:#04x}"),
        };
        if tx.send(message).is_err() {
            return;
        }
    }
}

impl Client {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_studio-server"))
            .arg("--mock")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn studio-server");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || read_all(stdout, tx));
        Self {
            stdin: child.stdin.take(),
            child,
            rx,
            next_id: 1,
            backlog: Vec::new(),
        }
    }

    fn next(&mut self, deadline: Instant) -> Message {
        let left = deadline.saturating_duration_since(Instant::now());
        self.rx
            .recv_timeout(left)
            .expect("message before the deadline")
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let body =
            serde_json::to_vec(&json!({ "id": id, "method": method, "params": params })).unwrap();
        let stdin = self.stdin.as_mut().unwrap();
        stdin
            .write_all(&(body.len() as u32 + 1).to_le_bytes())
            .unwrap();
        stdin.write_all(b"J").unwrap();
        stdin.write_all(&body).unwrap();
        stdin.flush().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.next(deadline) {
                Message::Json(v) if v["type"] == "response" && v["id"] == json!(id) => {
                    return if v["ok"] == json!(true) {
                        Ok(v["result"].clone())
                    } else {
                        Err(v["error"].as_str().unwrap_or_default().to_string())
                    };
                }
                other => self.backlog.push(other),
            }
        }
    }

    /// Wait for a message matching `want`, looking at the backlog first.
    fn wait(&mut self, what: &str, mut want: impl FnMut(&Message) -> bool) -> Message {
        if let Some(at) = self.backlog.iter().position(&mut want) {
            return self.backlog.remove(at);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let m = self.next(deadline);
            if want(&m) {
                return m;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
        }
    }
}

fn is_status(m: &Message, session: u32, state: &str) -> bool {
    matches!(m, Message::Json(v) if v["type"] == "event"
        && v["session"] == json!(session)
        && v["event"]["type"] == "status"
        && v["event"]["state"] == state)
}

#[test]
fn mock_session_over_stdio() {
    let mut client = Client::spawn();
    let ready = client.wait(
        "ready",
        |m| matches!(m, Message::Json(v) if v["type"] == "ready"),
    );
    let Message::Json(ready) = ready else {
        unreachable!()
    };
    assert_eq!(ready["mock"], json!(true));

    // A bad call answers with an error and leaves the server running
    let err = client.call("no_such_method", json!({})).unwrap_err();
    assert!(err.contains("bad request"), "{err}");
    assert!(client
        .call("session_discard", Value::Null)
        .unwrap_err()
        .contains("connect"));

    let opened = client
        .call("open_elf", json!({ "path": FIXTURE }))
        .expect("open_elf");
    let roots = opened["roots"].as_array().unwrap();
    let find = |name: &str| {
        roots
            .iter()
            .find(|r| r["path"] == name)
            .unwrap_or_else(|| panic!("{name} among the roots"))["ref"]
            .clone()
    };
    let counter = find("global_counter");
    let sensor = find("sensor_data");

    let probes = client.call("list_probes", Value::Null).unwrap();
    assert_eq!(probes[0]["selector"], "mock");

    let results = client
        .call(
            "session_set_watches",
            json!({ "watches": [
                { "id": 1, "node": counter, "cell": null },
                { "id": 2, "node": sensor, "cell": null },
            ]}),
        )
        .unwrap();
    assert_eq!(
        results,
        json!([{ "id": 1, "error": null }, { "id": 2, "error": null }])
    );

    let request = json!({
        "carrier": "probe", "probe": null, "chip": "STM32F407VG",
        "speedKhz": null, "port": null, "rateHz": 200.0,
    });
    client
        .call(
            "session_connect",
            json!({ "request": request, "session": 7 }),
        )
        .expect("connect");
    client.wait("connected", |m| is_status(m, 7, "connected"));

    let mut ticks = 0;
    let mut frames = 0;
    while frames < 5 {
        let Message::Frame { session, tts1 } =
            client.wait("a frame", |m| matches!(m, Message::Frame { .. }))
        else {
            unreachable!()
        };
        assert_eq!(session, 7);
        let word = |at: usize| u32::from_le_bytes(tts1[at..at + 4].try_into().unwrap());
        assert_eq!(word(0), TTS1);
        let (n, columns) = (word(4) as usize, word(8) as usize);
        assert_eq!(columns, 2);
        assert_eq!(tts1.len(), 16 + 8 * n + columns * (8 + 8 * n));
        let first_column = 16 + 8 * n;
        assert_eq!(word(first_column), 1);
        assert_eq!(word(first_column + 8 + 8 * n), 2);
        ticks += n;
        frames += 1;
    }
    assert!(ticks >= 5, "{ticks} ticks in 5 frames");

    client.wait(
        "stats",
        |m| matches!(m, Message::Json(v) if v["type"] == "event" && v["event"]["type"] == "stats"),
    );
    let read = client
        .call("session_read_values", json!({ "nodes": [counter] }))
        .unwrap();
    assert_eq!(read[0]["error"], Value::Null);

    client
        .call("session_set_rate", json!({ "hz": 50.0 }))
        .unwrap();
    client.call("session_disconnect", Value::Null).unwrap();
    client.wait("disconnected", |m| is_status(m, 7, "disconnected"));
    assert!(client
        .call("session_read_values", json!({ "nodes": [counter] }))
        .is_err());

    // Closing stdin shuts the server down
    drop(client.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = client.child.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(Instant::now() < deadline, "server did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn app_event(m: &Message, kind: &str) -> Option<Value> {
    match m {
        Message::Json(v) if v["type"] == "app_event" && v["event"]["type"] == kind => {
            Some(v["event"].clone())
        }
        _ => None,
    }
}

fn connect_mock(client: &mut Client, session: u32) {
    client
        .call("open_elf", json!({ "path": FIXTURE }))
        .expect("open_elf");
    let counter = json!({ "symbol": "global_counter", "steps": [] });
    let sensor = json!({ "symbol": "sensor_data", "steps": [] });
    client
        .call(
            "session_set_watches",
            json!({ "watches": [
                { "id": 1, "node": counter, "cell": null, "unit": "count" },
                { "id": 2, "node": sensor, "cell": null, "name": "sensor" },
            ]}),
        )
        .unwrap();
    let request = json!({
        "carrier": "probe", "probe": null, "chip": "STM32F407VG",
        "speedKhz": null, "port": null, "rateHz": 1000.0,
    });
    client
        .call(
            "session_connect",
            json!({ "request": request, "session": session }),
        )
        .expect("connect");
    client.wait("connected", |m| is_status(m, session, "connected"));
}

#[test]
fn recording_and_stream_over_stdio() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::spawn();
    client.wait(
        "ready",
        |m| matches!(m, Message::Json(v) if v["type"] == "ready"),
    );
    // Not before a session
    assert!(client
        .call("recording_start", json!({ "dir": dir.path() }))
        .unwrap_err()
        .contains("connect"));
    connect_mock(&mut client, 3);

    let started = client
        .call(
            "recording_start",
            json!({ "path": null, "dir": dir.path() }),
        )
        .unwrap();
    assert_eq!(started["active"], true);
    let path = started["path"].as_str().unwrap().to_string();
    assert!(path.starts_with(dir.path().to_str().unwrap()), "{path}");

    // The stream on a free port, read by a plain TCP client
    let stream = client
        .call("stream_start", json!({ "port": 0, "bindAll": false }))
        .unwrap();
    assert_eq!(stream["listening"], true);
    let address = stream["address"].as_str().unwrap().to_string();
    assert!(address.starts_with("127.0.0.1:"), "{address}");
    let socket = std::net::TcpStream::connect(&address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut lines = std::io::BufRead::lines(std::io::BufReader::new(socket));
    let hello: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["session"]["connected"], true);
    assert_eq!(hello["watches"][0]["unit"], "count");
    assert_eq!(hello["watches"][1]["name"], "sensor");
    let samples: Value = loop {
        let m: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        if m["type"] == "samples" {
            break m;
        }
    };
    assert!(samples["values"]["global_counter"].is_array());

    // Progress arrives while recording
    let progress = client.wait("recording progress", |m| {
        app_event(m, "recording")
            .is_some_and(|e| e["active"] == true && e["ticks"].as_u64() > Some(0))
    });
    let progress = app_event(&progress, "recording").unwrap();
    assert!(progress["bytes"].as_u64().unwrap() > 0);
    let state = client.call("app_state", Value::Null).unwrap();
    assert_eq!(state["recording"]["active"], true);
    assert_eq!(state["stream"]["clients"], 1);

    let stopped = client.call("recording_stop", Value::Null).unwrap();
    assert_eq!(stopped["active"], false);
    assert_eq!(stopped["dropped"], 0);
    let ticks = stopped["ticks"].as_u64().unwrap();
    assert!(
        ticks >= progress["ticks"].as_u64().unwrap(),
        "final tick count includes reported progress"
    );
    assert!(client
        .call("recording_stop", Value::Null)
        .unwrap_err()
        .contains("not recording"));

    let csv = client
        .call("export_csv", json!({ "mcapPath": path, "csvPath": null }))
        .unwrap();
    assert_eq!(csv["rows"].as_u64(), Some(ticks));
    assert_eq!(csv["truncated"], false);
    let text = std::fs::read_to_string(csv["path"].as_str().unwrap()).unwrap();
    assert_eq!(
        text.lines().next(),
        Some("time,global_counter [count],sensor")
    );

    // The stream stays up across a reconnect
    client.call("session_disconnect", Value::Null).unwrap();
    connect_mock(&mut client, 4);
    let mut kinds = Vec::new();
    loop {
        let m: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        kinds.push(m["type"].as_str().unwrap().to_string());
        if m["type"] == "samples" && kinds.iter().any(|k| k == "status") {
            break;
        }
    }
    let stopped = client.call("stream_stop", Value::Null).unwrap();
    assert_eq!(stopped["listening"], false);
    // The server closed the socket
    assert!(lines.all(|l| l.is_ok()));

    drop(client.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "server did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_killed_server_leaves_a_readable_recording() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::spawn();
    connect_mock(&mut client, 1);
    let path = dir.path().join("killed.mcap");
    client
        .call("recording_start", json!({ "path": path }))
        .unwrap();
    // Past two periodic flushes
    let mut last = 0;
    while last < 2000 {
        let m = client.wait("recording progress", |m| {
            app_event(m, "recording").is_some()
        });
        last = app_event(&m, "recording").unwrap()["ticks"]
            .as_u64()
            .unwrap();
    }
    client.child.kill().unwrap();
    client.child.wait().unwrap();

    let csv = studio_app::export_csv(&path, None).unwrap();
    println!(
        "killed after {last} ticks reported: {} rows readable, truncated {}",
        csv.rows, csv.truncated
    );
    assert!(csv.truncated);
    assert!(csv.rows >= 1500, "{} rows", csv.rows);
}

#[test]
fn can_poll_works_but_mock_server_never_opens_physical_adapters() {
    let mut client = Client::spawn();
    let snapshot = client
        .call(
            "can_request",
            json!({"request":{"action":"poll","after":0}}),
        )
        .unwrap();
    assert_eq!(snapshot["connected"], false);
    assert_eq!(snapshot["frames"], json!([]));
    let error = client
        .call(
            "can_request",
            json!({"request":{"action":"scan","library":null}}),
        )
        .unwrap_err();
    assert!(error.contains("--mock"));
    client.stdin.take();
    assert!(client.child.wait().unwrap().success());
}
