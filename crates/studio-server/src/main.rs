//! studio-server: the studio backend for the VS Code extension, as a sidecar
//! speaking the protocol in [`protocol`] on stdin and stdout. Logs go to stderr.
//!
//! `--mock` swaps the debug probe for an in-memory target holding the ELF's
//! initialised data, so the whole path runs without hardware.
//!
//! `--read-only` never writes target memory: RTT is left alone (reading the log
//! moves its read offset) and any write is refused and reported on stderr.
//!
//! The server exits when stdin closes, stopping any session first so the probe
//! is released.

mod protocol;

use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};
use studio_app::{
    ConnectRequest, SessionEvent, SessionSink, StreamOptions, StudioApp, WatchRequest,
};
use studio_carriers::mock::MockLink;
use studio_carriers::probe::{ProbeConfig, ProbeInfo};
use studio_carriers::{CarrierError, Link, MemoryAccess};
use studio_core::catalog::ElfImage;
use studio_dwarf::NodeRef;

use protocol::{encode, encode_frame, read_message, KIND_JSON};

/// Messages waiting for stdout. Frames beyond this are dropped (the session
/// counts them); responses and events wait for room.
const OUTBOX: usize = 256;

#[derive(Clone)]
struct Out(SyncSender<Vec<u8>>);

impl Out {
    fn json(&self, value: &Value) {
        let bytes = serde_json::to_vec(value).expect("JSON values serialise");
        let _ = self.0.send(encode(KIND_JSON, &bytes));
    }

    fn frame(&self, session: u32, tts1: &[u8]) -> bool {
        match self.0.try_send(encode_frame(session, tts1)) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }

    fn respond(&self, id: Value, result: Result<Value, String>) {
        self.json(&match result {
            Ok(result) => json!({ "type": "response", "id": id, "ok": true, "result": result }),
            Err(error) => json!({ "type": "response", "id": id, "ok": false, "error": error }),
        });
    }
}

/// Delivers one session's output, tagged with the id the client gave it.
struct StdioSink {
    out: Out,
    session: u32,
}

impl SessionSink for StdioSink {
    fn frame(&self, bytes: Vec<u8>) -> bool {
        self.out.frame(self.session, &bytes)
    }

    fn event(&self, event: SessionEvent) {
        self.out
            .json(&json!({ "type": "event", "session": self.session, "event": event }));
    }
}

#[derive(Deserialize)]
struct Envelope {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

/// The methods, with the Tauri commands' names and arguments.
#[derive(Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
enum Call {
    CanRequest {
        request: studio_app::can::Request,
    },
    OpenElf {
        path: PathBuf,
    },
    SymbolChildren {
        node: NodeRef,
        limit: Option<usize>,
    },
    StartupElfPath {},
    ListProbes {},
    SearchChips {
        query: String,
    },
    ListSerialPorts {},
    SessionConnect {
        request: ConnectRequest,
        session: u32,
    },
    SessionDisconnect {},
    SessionSetWatches {
        watches: Vec<WatchRequest>,
    },
    SessionSetRate {
        hz: f64,
    },
    SessionRequest {
        id: u32,
        value: f64,
    },
    SessionDiscard {},
    SessionSave {},
    WatchableLeaves {
        node: NodeRef,
    },
    SessionTaskStates {},
    SessionTaskTrace {},
    InspectChildren {
        node: NodeRef,
        offset: u64,
        limit: usize,
        live: bool,
    },
    NodeMetadata {
        node: NodeRef,
    },
    SessionReadValues {
        nodes: Vec<NodeRef>,
    },
    /// `dir` is where a recording without a `path` goes; the extension passes
    /// the workspace's `.tuning-studio/recordings`
    #[serde(rename_all = "camelCase")]
    RecordingStart {
        path: Option<PathBuf>,
        dir: Option<PathBuf>,
    },
    RecordingStop {},
    #[serde(rename_all = "camelCase")]
    ExportCsv {
        mcap_path: PathBuf,
        csv_path: Option<PathBuf>,
    },
    #[serde(rename_all = "camelCase")]
    StreamStart {
        port: Option<u16>,
        #[serde(default)]
        bind_all: bool,
    },
    StreamStop {},
    AppState {},
}

impl Call {
    /// Quick calls answer in order on the reader thread, as the Tauri app runs
    /// its synchronous commands; the rest may block and get a thread each.
    fn is_quick(&self) -> bool {
        matches!(
            self,
            Call::SymbolChildren { .. }
                | Call::StartupElfPath {}
                | Call::SessionSetWatches { .. }
                | Call::SessionSetRate { .. }
                | Call::WatchableLeaves { .. }
                | Call::AppState {}
        )
    }
}

fn to_value<T: serde::Serialize>(result: Result<T, String>) -> Result<Value, String> {
    result.and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string()))
}

struct Server {
    app: Arc<StudioApp>,
    out: Out,
    mock: bool,
    read_only: bool,
}

impl Server {
    fn run(&self, call: Call) -> Result<Value, String> {
        let app = &self.app;
        match call {
            Call::CanRequest { request } => {
                use studio_app::can::Request;
                if self.mock && !matches!(request, Request::Poll { .. } | Request::Disconnect) {
                    return Err("Physical CAN access is disabled in --mock mode".into());
                }
                if self.read_only
                    && matches!(request, Request::Connect { .. } | Request::Send { .. })
                {
                    return Err("CAN connection/transmission is disabled in --read-only mode: the SDK has no silent mode".into());
                }
                to_value(app.can_request(request))
            }
            Call::OpenElf { path } => to_value(app.open_elf(path)),
            Call::SymbolChildren { node, limit } => to_value(app.symbol_children(&node, limit)),
            Call::StartupElfPath {} => to_value(Ok(studio_app::startup_elf_path())),
            Call::ListProbes {} if self.mock => to_value(Ok(vec![mock_probe_info()])),
            Call::ListProbes {} => to_value(Ok(studio_app::list_probes())),
            Call::SearchChips { query } => to_value(Ok(studio_app::search_chips(&query))),
            Call::ListSerialPorts {} => to_value(Ok(studio_app::list_serial_ports())),
            Call::SessionConnect { request, session } => {
                let sink = Arc::new(StdioSink {
                    out: self.out.clone(),
                    session,
                });
                to_value(app.connect(request, sink))
            }
            Call::SessionDisconnect {} => {
                app.disconnect();
                Ok(Value::Null)
            }
            Call::SessionSetWatches { watches } => to_value(app.set_watches(watches)),
            Call::SessionSetRate { hz } => {
                app.set_rate(hz);
                Ok(Value::Null)
            }
            Call::SessionRequest { id, value } => to_value(app.request(id, value)),
            Call::SessionDiscard {} => to_value(app.discard()),
            Call::SessionSave {} => to_value(app.save()),
            Call::WatchableLeaves { node } => to_value(app.watchable_leaves(&node)),
            Call::InspectChildren {
                node,
                offset,
                limit,
                live,
            } => to_value(app.inspect_children(&node, offset, limit, live)),
            Call::NodeMetadata { node } => to_value(app.node_metadata(&node)),
            Call::SessionTaskTrace {} => to_value(app.task_trace()),
            Call::SessionTaskStates {} => to_value(app.task_states()),
            Call::SessionReadValues { nodes } => to_value(app.read_values(&nodes)),
            Call::RecordingStart { path, dir } => to_value(app.start_recording(path, dir)),
            Call::RecordingStop {} => to_value(app.stop_recording()),
            Call::ExportCsv {
                mcap_path,
                csv_path,
            } => to_value(studio_app::export_csv(&mcap_path, csv_path.as_deref())),
            Call::StreamStart { port, bind_all } => to_value(app.start_stream(StreamOptions::new(
                port.unwrap_or(studio_app::DEFAULT_PORT),
                bind_all,
            ))),
            Call::StreamStop {} => to_value(Ok(app.stop_stream())),
            Call::AppState {} => to_value(Ok(app.app_state())),
        }
    }
}

fn parse(body: &[u8]) -> Result<(Value, Result<Call, String>), String> {
    let envelope: Envelope =
        serde_json::from_slice(body).map_err(|e| format!("bad request: {e}"))?;
    let params = match envelope.params {
        Value::Null => json!({}),
        p => p,
    };
    let call = serde_json::from_value(json!({ "method": envelope.method, "params": params }))
        .map_err(|e| format!("bad request for `{}`: {e}", envelope.method));
    Ok((envelope.id, call))
}

fn mock_probe_info() -> ProbeInfo {
    ProbeInfo {
        selector: "mock".into(),
        name: "Mock target".into(),
        serial: None,
    }
}

/// An in-memory target holding the ELF's initialised sections.
fn open_mock(_: &ProbeConfig, elf: &Path) -> Result<Box<dyn Link>, CarrierError> {
    let bytes = std::fs::read(elf).map_err(|e| CarrierError::Other(e.to_string()))?;
    let image = ElfImage::parse(&bytes).map_err(CarrierError::Other)?;
    let mock = MockLink::new();
    for (address, data) in image.sections() {
        mock.poke(address, data);
    }
    Ok(Box::new(mock))
}

/// Refused writes, over every session of a `--read-only` server
static REFUSED_WRITES: AtomicU64 = AtomicU64::new(0);

/// A carrier whose memory refuses writes.
struct ReadOnlyLink(Box<dyn Link>);

struct ReadOnlyMemory<'a>(&'a mut dyn MemoryAccess);

impl Link for ReadOnlyLink {
    fn with_memory(
        &mut self,
        body: &mut dyn FnMut(&mut dyn MemoryAccess),
    ) -> studio_carriers::Result<()> {
        self.0
            .with_memory(&mut |memory| body(&mut ReadOnlyMemory(memory)))
    }
}

impl MemoryAccess for ReadOnlyMemory<'_> {
    fn read(&mut self, address: u64, buf: &mut [u8]) -> studio_carriers::Result<()> {
        self.0.read(address, buf)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> studio_carriers::Result<()> {
        let n = REFUSED_WRITES.fetch_add(1, Ordering::Relaxed) + 1;
        eprintln!(
            "studio-server: refused write of {} bytes at {address:#010x} (--read-only, {n} so far)",
            data.len()
        );
        Err(CarrierError::Write {
            address,
            len: data.len(),
            reason: "studio-server runs --read-only".into(),
        })
    }
}

fn write_out(rx: Receiver<Vec<u8>>) {
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    while let Ok(first) = rx.recv() {
        // Flush once the queue is drained, so bursts go out in one write
        for message in std::iter::once(first).chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            if out.write_all(&message).is_err() {
                return;
            }
        }
        if out.flush().is_err() {
            return;
        }
    }
}

fn main() {
    if std::env::args().any(|a| a == studio_carriers::can_process::WORKER_FLAG) {
        studio_carriers::can_process::run_worker_stdio();
    }
    let mock = std::env::args().any(|a| a == "--mock");
    let read_only = std::env::args().any(|a| a == "--read-only");
    if std::env::args().any(|a| a == "--version") {
        println!("studio-server {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // hidapi on macOS binds to the run loop of the first thread that initialises
    // it; this thread lives as long as the process
    if !mock {
        studio_carriers::probe::init_hid_on_main_thread();
    }

    let (tx, rx) = mpsc::sync_channel(OUTBOX);
    let writer = std::thread::Builder::new()
        .name("stdout".into())
        .spawn(move || write_out(rx))
        .expect("spawn stdout thread");
    let out = Out(tx);
    let app = if mock {
        StudioApp::with_probe_opener(Arc::new(open_mock))
    } else {
        StudioApp::new()
    };
    let app = Arc::new(if read_only {
        let open = app.probe_opener();
        StudioApp::with_probe_opener(Arc::new(move |config, elf| {
            open(config, elf).map(|link| Box::new(ReadOnlyLink(link)) as Box<dyn Link>)
        }))
        .without_rtt()
    } else {
        app
    });
    let events = out.clone();
    app.set_event_sink(Arc::new(move |event| {
        events.json(&json!({ "type": "app_event", "event": event }));
    }));
    let server = Arc::new(Server {
        app: app.clone(),
        out: out.clone(),
        mock,
        read_only,
    });
    out.json(&json!({
        "type": "ready",
        "version": env!("CARGO_PKG_VERSION"),
        "mock": mock,
        "readOnly": read_only,
    }));

    let mut input = BufReader::new(io::stdin().lock());
    loop {
        let (kind, body) = match read_message(&mut input) {
            Ok(Some(message)) => message,
            Ok(None) => break,
            Err(e) => {
                eprintln!("studio-server: {e}");
                break;
            }
        };
        if kind != KIND_JSON {
            eprintln!("studio-server: ignoring message of kind {kind:#04x}");
            continue;
        }
        let (id, call) = match parse(&body) {
            Ok(parsed) => parsed,
            Err(e) => {
                eprintln!("studio-server: {e}");
                continue;
            }
        };
        let call = match call {
            Ok(call) => call,
            Err(e) => {
                out.respond(id, Err(e));
                continue;
            }
        };
        if call.is_quick() {
            out.respond(id, server.run(call));
        } else {
            let server = server.clone();
            std::thread::spawn(move || {
                let result = server.run(call);
                server.out.respond(id, result);
            });
        }
    }

    // Stdin closed: the extension is gone or wants us gone. Let go of the probe
    // first; that also closes a recording. Then close the stream's port.
    let _ = app.can_request(studio_app::can::Request::Disconnect);
    app.disconnect();
    app.stop_stream();
    if read_only {
        let refused = REFUSED_WRITES.load(Ordering::Relaxed);
        eprintln!("studio-server: exiting; {refused} writes refused");
    }
    // Request threads may still hold senders, so the writer is not joined; give it
    // a moment to pass on the session's last events
    drop((server, out, writer));
    std::thread::sleep(std::time::Duration::from_millis(50));
}
