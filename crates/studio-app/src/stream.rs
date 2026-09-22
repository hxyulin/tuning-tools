//! The live stream: a TCP server sending newline-delimited JSON to the user's
//! own scripts. The protocol is specified in `docs/stream.md`.
//!
//! Threads: one accepts connections; one (the fan-out) takes messages from the
//! [`Tap`], encodes each once and offers it to every client's bounded queue; each
//! client has a writer, and a reader that discards what the client sends and
//! notices when it goes away. A client whose queue is full loses messages (and
//! is told how many); one that stays full past a limit is disconnected. Neither
//! reaches other clients or the session.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};
use studio_core::session::LinkState;
use studio_core::tap::{SessionInfo, Subscription, TapEvent, TapSnapshot};
use studio_core::tune::{CatalogCheck, TuneValue};
use studio_core::{SampleBatch, SessionEvent, Tap, WatchSet};

use crate::{AppEvent, AppEventSink, StudioApp};

pub const DEFAULT_PORT: u16 = 7878;
pub const PROTOCOL_VERSION: u32 = 1;
/// Tap messages the fan-out may fall behind by
const TAP_QUEUE: usize = 1024;
/// How often the UI hears about clients and drops when nothing else changes
const STATE_EVERY: Duration = Duration::from_secs(1);
const IO_RETRY: Duration = Duration::from_millis(5);
const CLIENT_IDLE: Duration = Duration::from_millis(20);

#[derive(Debug, Clone)]
pub struct StreamOptions {
    pub port: u16,
    /// Listen on every interface instead of 127.0.0.1 only
    pub bind_all: bool,
    /// Messages queued per client
    pub client_queue: usize,
    /// Sample batches a client may lose in a row before it is disconnected
    pub drop_limit: u64,
}

impl StreamOptions {
    pub fn new(port: u16, bind_all: bool) -> Self {
        Self {
            port,
            bind_all,
            client_queue: 256,
            // About five seconds of batches
            drop_limit: 150,
        }
    }
}

/// The stream as the UI shows it.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StreamState {
    pub listening: bool,
    /// `127.0.0.1:7878`
    pub address: Option<String>,
    pub bind_all: bool,
    pub clients: usize,
    /// Sample batches lost over every client, those disconnected included
    pub dropped: u64,
    pub error: Option<String>,
}

impl StreamState {
    pub fn stopped() -> Self {
        Self {
            listening: false,
            address: None,
            bind_all: false,
            clients: 0,
            dropped: 0,
            error: None,
        }
    }
}

type Line = Arc<[u8]>;

pub(crate) struct StreamServer {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    state: Arc<Mutex<StreamState>>,
}

impl StreamServer {
    pub(crate) fn start(
        tap: &Arc<Tap>,
        options: StreamOptions,
        events: Option<AppEventSink>,
    ) -> Result<Self, String> {
        let ip = if options.bind_all {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        };
        let listener = TcpListener::bind((ip, options.port))
            .map_err(|e| format!("could not listen on {ip}:{}: {e}", options.port))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let local = listener.local_addr().map_err(|e| e.to_string())?;
        let state = Arc::new(Mutex::new(StreamState {
            listening: true,
            address: Some(local.to_string()),
            bind_all: options.bind_all,
            clients: 0,
            dropped: 0,
            error: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (subscription, snapshot) = tap.subscribe(TAP_QUEUE);
        let (accepted, arrivals) = mpsc::channel();

        let accept_stop = stop.clone();
        let acceptor = std::thread::Builder::new()
            .name("stream-accept".into())
            .spawn(move || accept(listener, accepted, &accept_stop))
            .map_err(|e| e.to_string())?;
        let fanout = Fanout::new(snapshot, options, state.clone(), events);
        let fanout_stop = stop.clone();
        let fanout = std::thread::Builder::new()
            .name("stream-fanout".into())
            .spawn(move || fanout.run(subscription, arrivals, &fanout_stop))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            stop,
            threads: vec![acceptor, fanout],
            state,
        })
    }

    pub(crate) fn state(&self) -> StreamState {
        self.state.lock().expect("stream state poisoned").clone()
    }

    pub(crate) fn local_addr(&self) -> Option<SocketAddr> {
        self.state().address.and_then(|a| a.parse().ok())
    }
}

impl Drop for StreamServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn accept(listener: TcpListener, accepted: mpsc::Sender<TcpStream>, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_nodelay(true);
                if accepted.send(stream).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                tracing::warn!(%e, "stream accept failed");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

struct Client {
    queue: SyncSender<Line>,
    /// A handle to shut the socket down, which unblocks its writer
    socket: TcpStream,
    alive: Arc<AtomicBool>,
    writer: JoinHandle<()>,
    reader: JoinHandle<()>,
    /// Lost since the client last got a message, and not reported to it yet
    pending_batches: u64,
    pending_messages: u64,
    dropped: u64,
}

impl Client {
    fn spawn(socket: TcpStream, queue_len: usize) -> std::io::Result<Self> {
        socket.set_nonblocking(true)?;
        let (queue, lines) = mpsc::sync_channel::<Line>(queue_len.max(2));
        let alive = Arc::new(AtomicBool::new(true));
        let mut write_half = socket.try_clone()?;
        let mut read_half = socket.try_clone()?;
        let writer_alive = alive.clone();
        let writer = std::thread::Builder::new()
            .name("stream-client-write".into())
            .spawn(move || {
                write_lines(&mut write_half, &lines, &writer_alive);
                writer_alive.store(false, Ordering::Relaxed);
                let _ = write_half.shutdown(Shutdown::Both);
            })?;
        let reader_alive = alive.clone();
        let reader = std::thread::Builder::new()
            .name("stream-client-read".into())
            .spawn(move || {
                // Input is ignored; reading only notices the client leaving
                let mut buf = [0u8; 1024];
                while reader_alive.load(Ordering::Relaxed) {
                    match read_half.read(&mut buf) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(e) if e.kind() == ErrorKind::Interrupted => {}
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {
                            std::thread::sleep(CLIENT_IDLE);
                        }
                        Err(_) => break,
                    }
                }
                reader_alive.store(false, Ordering::Relaxed);
                let _ = read_half.shutdown(Shutdown::Both);
            })?;
        Ok(Self {
            queue,
            socket,
            alive,
            writer,
            reader,
            pending_batches: 0,
            pending_messages: 0,
            dropped: 0,
        })
    }

    /// Offer `line`; `false` when the client should be disconnected.
    fn offer(&mut self, line: &Line, batch: bool, drop_limit: u64) -> bool {
        if !self.alive.load(Ordering::Relaxed) {
            return false;
        }
        if self.pending_batches + self.pending_messages > 0 {
            let notice = json_line(&json!({
                "type": "dropped",
                "batches": self.pending_batches,
                "messages": self.pending_messages,
            }));
            match self.queue.try_send(notice) {
                Ok(()) => {
                    self.pending_batches = 0;
                    self.pending_messages = 0;
                }
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        match self.queue.try_send(line.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                if batch {
                    self.pending_batches += 1;
                    self.dropped += 1;
                } else {
                    self.pending_messages += 1;
                }
                self.pending_batches <= drop_limit
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    fn close(self) {
        self.alive.store(false, Ordering::Relaxed);
        let _ = self.socket.shutdown(Shutdown::Both);
        drop(self.queue);
        let _ = self.writer.join();
        let _ = self.reader.join();
    }
}

fn write_lines(socket: &mut TcpStream, lines: &Receiver<Line>, alive: &AtomicBool) {
    while alive.load(Ordering::Relaxed) {
        match lines.recv_timeout(CLIENT_IDLE) {
            Ok(line) => {
                if !write_line(socket, &line, alive) {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

// Keep the offset across partial writes; dropping a buffered writer must not
// attempt another blocking flush while the fan-out is joining this thread.
fn write_line(out: &mut impl Write, mut bytes: &[u8], alive: &AtomicBool) -> bool {
    while !bytes.is_empty() {
        if !alive.load(Ordering::Relaxed) {
            return false;
        }
        match out.write(bytes) {
            Ok(0) => return false,
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(IO_RETRY);
            }
            Err(_) => return false,
        }
    }
    true
}

fn json_line(value: &Value) -> Line {
    let mut bytes = serde_json::to_vec(value).expect("JSON values serialise");
    bytes.push(b'\n');
    bytes.into()
}

/// Session metadata as the stream sends it
fn session_json(
    info: Option<&SessionInfo>,
    link: LinkState,
    clock: Option<SystemTime>,
    check: Option<&CatalogCheck>,
) -> Value {
    let mut session = info
        .and_then(|i| serde_json::to_value(i).ok())
        .unwrap_or_else(|| json!({}));
    session["state"] = json!(link);
    session["connected"] = json!(link == LinkState::Connected);
    session["startedAt"] = clock.map_or(Value::Null, |at| {
        json!(chrono::DateTime::<chrono::Local>::from(at).to_rfc3339())
    });
    session["startedAtUnixNs"] = clock
        .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
        .map_or(Value::Null, |d| json!(d.as_nanos() as u64));
    session["elfMatch"] = check.map_or(Value::Null, |c| json!(c));
    session
}

/// `{"type":"samples",...}`: one message per flushed batch, NaN as `null`
fn samples_line(seq: u64, batch: &SampleBatch, set: &WatchSet) -> Line {
    let mut out = Vec::with_capacity(64 + batch.values.len() * 12 + batch.ticks() * 12);
    let mut number = ryu::Buffer::new();
    let mut push = |out: &mut Vec<u8>, v: f64| {
        if v.is_finite() {
            out.extend_from_slice(number.format_finite(v).as_bytes());
        } else {
            out.extend_from_slice(b"null");
        }
    };
    out.extend_from_slice(format!("{{\"type\":\"samples\",\"seq\":{seq},\"t\":[").as_bytes());
    for (i, &t) in batch.times.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        push(&mut out, t);
    }
    out.extend_from_slice(b"],\"values\":{");
    for (col, w) in set.watches().iter().enumerate().take(batch.ids.len()) {
        if col > 0 {
            out.push(b',');
        }
        serde_json::to_writer(&mut out, &w.name).expect("JSON string");
        out.extend_from_slice(b":[");
        for (i, v) in batch.column(col).enumerate() {
            if i > 0 {
                out.push(b',');
            }
            push(&mut out, v);
        }
        out.push(b']');
    }
    out.extend_from_slice(b"}}\n");
    out.into()
}

struct Fanout {
    options: StreamOptions,
    clients: Vec<Client>,
    watches: Arc<WatchSet>,
    info: Option<Arc<SessionInfo>>,
    link: LinkState,
    clock: Option<SystemTime>,
    check: Option<CatalogCheck>,
    names: HashMap<u32, String>,
    tune: HashMap<u32, TuneValue>,
    seq: u64,
    /// Batches lost by clients already disconnected
    dropped_gone: u64,
    /// Tap drops already reported to clients
    tap_dropped: u64,
    state: Arc<Mutex<StreamState>>,
    events: Option<AppEventSink>,
}

impl Fanout {
    fn new(
        snapshot: TapSnapshot,
        options: StreamOptions,
        state: Arc<Mutex<StreamState>>,
        events: Option<AppEventSink>,
    ) -> Self {
        Self {
            options,
            clients: Vec::new(),
            names: snapshot
                .info
                .as_ref()
                .map(|i| i.tunables.iter().map(|t| (t.id, t.name.clone())).collect())
                .unwrap_or_default(),
            watches: snapshot.watches,
            info: snapshot.info,
            link: snapshot.link,
            clock: snapshot.clock,
            check: snapshot.check,
            tune: HashMap::new(),
            seq: 0,
            dropped_gone: 0,
            tap_dropped: 0,
            state,
            events,
        }
    }

    fn run(mut self, tap: Subscription, arrivals: Receiver<TcpStream>, stop: &AtomicBool) {
        let mut next_state = Instant::now() + STATE_EVERY;
        let mut reported = self.publish_state(None);
        while !stop.load(Ordering::Relaxed) {
            let mut changed = false;
            while let Ok(socket) = arrivals.try_recv() {
                changed |= self.add(socket);
            }
            match tap.recv_timeout(Duration::from_millis(20)) {
                Ok(event) => {
                    let lost = tap.drops().batches();
                    if lost > self.tap_dropped {
                        // The fan-out itself fell behind: every client missed these
                        let n = lost - self.tap_dropped;
                        self.tap_dropped = lost;
                        let line =
                            json_line(&json!({ "type": "dropped", "batches": n, "messages": 0 }));
                        self.broadcast(&line, false);
                    }
                    self.handle(event);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            let before = self.clients.len();
            self.reap();
            changed |= self.clients.len() != before;
            if changed || Instant::now() >= next_state {
                next_state = Instant::now() + STATE_EVERY;
                let state = self.current_state();
                if changed || state != reported {
                    reported = self.publish_state(Some(state));
                }
            }
        }
        for client in self.clients.drain(..) {
            client.close();
        }
    }

    fn current_state(&self) -> StreamState {
        let mut state = self.state.lock().expect("stream state poisoned").clone();
        state.clients = self.clients.len();
        state.dropped = self.dropped_gone
            + self.tap_dropped
            + self.clients.iter().map(|c| c.dropped).sum::<u64>();
        state
    }

    fn publish_state(&self, state: Option<StreamState>) -> StreamState {
        let state = state.unwrap_or_else(|| self.current_state());
        *self.state.lock().expect("stream state poisoned") = state.clone();
        if let Some(events) = &self.events {
            events(AppEvent::Stream(state.clone()));
        }
        state
    }

    /// Greet a new client; `true` when it was added.
    fn add(&mut self, socket: TcpStream) -> bool {
        let Ok(mut client) = Client::spawn(socket, self.options.client_queue) else {
            return false;
        };
        let hello = json_line(&json!({
            "type": "hello",
            "version": PROTOCOL_VERSION,
            "session": self.session(),
            "watches": &*self.watches,
        }));
        client.offer(&hello, false, u64::MAX);
        self.clients.push(client);
        true
    }

    fn session(&self) -> Value {
        session_json(
            self.info.as_deref(),
            self.link,
            self.clock,
            self.check.as_ref(),
        )
    }

    /// Drop clients that left or were disconnected.
    fn reap(&mut self) {
        let mut i = 0;
        while i < self.clients.len() {
            if self.clients[i].alive.load(Ordering::Relaxed) {
                i += 1;
            } else {
                let client = self.clients.swap_remove(i);
                self.dropped_gone += client.dropped;
                client.close();
            }
        }
    }

    fn broadcast(&mut self, line: &Line, batch: bool) {
        let limit = self.options.drop_limit;
        for client in &mut self.clients {
            if !client.offer(line, batch, limit) {
                // Too far behind: shut it down; `reap` collects it
                client.alive.store(false, Ordering::Relaxed);
                let _ = client.socket.shutdown(Shutdown::Both);
            }
        }
    }

    fn set_watches(&mut self, set: &Arc<WatchSet>) {
        if *self.watches == **set {
            return;
        }
        self.watches = set.clone();
        let line = json_line(&json!({ "type": "watches", "watches": &**set }));
        self.broadcast(&line, false);
    }

    fn handle(&mut self, event: TapEvent) {
        match event {
            TapEvent::Samples { batch, watches } => {
                self.set_watches(&watches);
                self.seq += 1;
                let line = samples_line(self.seq, &batch, &watches);
                self.broadcast(&line, true);
            }
            TapEvent::Watches(set) => self.set_watches(&set),
            TapEvent::Info(info) => {
                self.names = info
                    .tunables
                    .iter()
                    .map(|t| (t.id, t.name.clone()))
                    .collect();
                self.info = Some(info);
                self.clock = None;
                self.check = None;
                self.tune.clear();
            }
            TapEvent::Clock(at) => self.clock = Some(at),
            TapEvent::TuneRequest(request) => {
                let line = json_line(&json!({
                    "type": "tune",
                    "kind": "request",
                    "action": request.action,
                    "id": request.id,
                    "name": request.id.and_then(|id| self.names.get(&id)),
                    "value": request.value,
                    "ok": request.error.is_none(),
                    "error": request.error,
                }));
                self.broadcast(&line, false);
            }
            TapEvent::Event(event) => self.session_event(&event),
        }
    }

    fn session_event(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::Status { state, message } => {
                self.link = *state;
                let line = json_line(&json!({
                    "type": "status",
                    "state": state,
                    "message": message,
                    "session": self.session(),
                }));
                self.broadcast(&line, false);
            }
            SessionEvent::Log { lines } => {
                let line = json_line(&json!({ "type": "log", "lines": lines }));
                self.broadcast(&line, false);
            }
            SessionEvent::Tune { check, values } => {
                let check_changed = self.check.as_ref() != Some(check);
                self.check = Some(check.clone());
                let changed = values.len() != self.tune.len()
                    || values.iter().any(|v| self.tune.get(&v.id) != Some(v));
                if !changed && !check_changed {
                    return;
                }
                self.tune = values.iter().map(|v| (v.id, v.clone())).collect();
                let values: Vec<Value> = values
                    .iter()
                    .map(|v| {
                        json!({
                            "id": v.id,
                            "name": self.names.get(&v.id),
                            "requested": v.requested,
                            "applied": v.applied,
                        })
                    })
                    .collect();
                let line = json_line(&json!({
                    "type": "tune",
                    "kind": "values",
                    "check": check,
                    "values": values,
                }));
                self.broadcast(&line, false);
            }
            SessionEvent::Catalog { catalog } => {
                self.names = catalog
                    .entries
                    .iter()
                    .map(|e| (e.id, e.name.clone()))
                    .collect();
            }
            SessionEvent::Stats { .. } => {}
        }
    }
}

impl StudioApp {
    /// Serve the live stream, replacing a running one.
    pub fn start_stream(&self, options: StreamOptions) -> Result<StreamState, String> {
        let mut slot = self.stream.lock().expect("stream poisoned");
        // The old listener must let go of the port first
        drop(slot.take());
        match StreamServer::start(&self.tap, options, self.event_sink()) {
            Ok(server) => {
                let state = server.state();
                *slot = Some(server);
                Ok(state)
            }
            Err(error) => {
                if let Some(events) = self.event_sink() {
                    events(AppEvent::Stream(StreamState {
                        error: Some(error.clone()),
                        ..StreamState::stopped()
                    }));
                }
                Err(error)
            }
        }
    }

    /// Stop the stream and disconnect its clients.
    pub fn stop_stream(&self) -> StreamState {
        let server = self.stream.lock().expect("stream poisoned").take();
        let mut state = server
            .as_ref()
            .map_or_else(StreamState::stopped, |s| s.state());
        drop(server);
        state.listening = false;
        state.clients = 0;
        if let Some(events) = self.event_sink() {
            events(AppEvent::Stream(state.clone()));
        }
        state
    }

    /// Where the stream listens, when it runs
    pub fn stream_address(&self) -> Option<SocketAddr> {
        self.stream
            .lock()
            .expect("stream poisoned")
            .as_ref()
            .and_then(StreamServer::local_addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (b, _) = listener.accept().unwrap();
        (a, b)
    }

    #[test]
    fn a_full_client_loses_batches_is_told_and_is_cut_off_past_the_limit() {
        let (_peer, socket) = pair();
        let mut client = Client::spawn(socket, 2).unwrap();
        // No writer can drain a queue this small faster than we fill it with
        // large lines while the peer never reads
        let big: Line = vec![b'x'; 4 << 20].into();
        let mut kept = true;
        let mut offered = 0;
        while kept && offered < 1000 {
            kept = client.offer(&big, true, 10);
            offered += 1;
        }
        assert!(!kept, "cut off after 10 lost batches in a row");
        assert_eq!(client.pending_batches, 11);
        assert!(client.dropped >= 11);
        client.close();
    }

    #[test]
    fn partial_writes_resume_without_repeating_bytes() {
        struct Partial {
            calls: usize,
            received: Vec<u8>,
        }
        impl Write for Partial {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.calls += 1;
                match self.calls {
                    2 => return Err(ErrorKind::WouldBlock.into()),
                    4 => return Err(ErrorKind::Interrupted.into()),
                    _ => {}
                }
                let n = bytes.len().min(2);
                self.received.extend_from_slice(&bytes[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                panic!("socket writes must not depend on flushing a buffer")
            }
        }
        let mut out = Partial {
            calls: 0,
            received: Vec::new(),
        };
        assert!(write_line(&mut out, b"example\n", &AtomicBool::new(true)));
        assert_eq!(out.received, b"example\n");
    }

    #[test]
    fn cancelling_a_stalled_write_stops_retrying() {
        struct Stalled<'a>(&'a AtomicBool);
        impl Write for Stalled<'_> {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                self.0.store(false, Ordering::Relaxed);
                Err(ErrorKind::WouldBlock.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                unreachable!()
            }
        }
        let alive = AtomicBool::new(true);
        assert!(!write_line(&mut Stalled(&alive), b"pending", &alive));
    }

    #[test]
    fn samples_are_columns_by_name_with_null_for_nan() {
        let set = WatchSet::new(vec![
            studio_core::WatchMeta {
                id: 1,
                name: "a".into(),
                path: "a".into(),
                type_name: "f32".into(),
                unit: None,
            },
            studio_core::WatchMeta {
                id: 2,
                name: "b".into(),
                path: "b".into(),
                type_name: "u8".into(),
                unit: Some("rpm".into()),
            },
        ]);
        let batch = SampleBatch {
            ids: vec![1, 2],
            times: vec![0.5, 0.75],
            values: vec![1.0, f64::NAN, 2.5, 3.0],
        };
        let line = samples_line(7, &batch, &set);
        let v: Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(
            v,
            json!({"type":"samples","seq":7,"t":[0.5,0.75],"values":{"a":[1.0,2.5],"b":[null,3.0]}})
        );
        assert_eq!(line.last(), Some(&b'\n'));
    }
}
