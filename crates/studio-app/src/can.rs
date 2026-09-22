//! Independent CAN session. A single worker owns the vendor SDK and recordings;
//! callbacks never do disk I/O or wait for the UI. Capture continues when hidden.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::PathBuf,
    sync::{
        mpsc::{self, Sender},
        Mutex,
    },
    thread,
    time::Duration,
};
use studio_carriers::can::{ChannelConfig, Device, Frame, Transmit};
use studio_carriers::can_process::RemoteAdapter;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum Request {
    Scan {
        library: Option<PathBuf>,
    },
    Connect {
        index: i32,
        channels: u8,
        configs: Vec<ChannelConfig>,
    },
    Disconnect,
    Send {
        frame: Transmit,
    },
    Poll {
        after: u64,
    },
    Record {
        path: PathBuf,
    },
    StopRecording,
}
#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub connected: bool,
    pub devices: Vec<Device>,
    pub configs: Vec<ChannelConfig>,
    pub frames: Vec<Frame>,
    pub sequence: u64,
    pub received: u64,
    pub transmitted: u64,
    pub errors: u64,
    pub dropped: u64,
    pub skipped: u64,
    pub recording: Option<String>,
    pub recorded: u64,
    pub error: Option<String>,
}
type Reply = Sender<Result<Snapshot, String>>;
struct Worker {
    tx: Sender<(Request, Reply)>,
    join: thread::JoinHandle<()>,
}
#[derive(Default)]
pub(crate) struct CanService {
    worker: Mutex<Option<Worker>>,
    library: Mutex<Option<PathBuf>>,
    worker_executable: Mutex<Option<PathBuf>>,
}
impl CanService {
    fn request(&self, request: Request) -> Result<Snapshot, String> {
        let mut worker = self.worker.lock().map_err(|_| "CAN worker lock poisoned")?;
        if worker.is_none() {
            let executable = self
                .worker_executable
                .lock()
                .map_err(|_| "CAN worker executable lock poisoned")?
                .clone();
            let (tx, rx) = mpsc::channel::<(Request, Reply)>();
            let join = thread::Builder::new()
                .name("can-capture".into())
                .spawn(move || {
                    let mut state = Capture {
                        worker_executable: executable,
                        ..Capture::default()
                    };
                    loop {
                        state.drain();
                        match rx.recv_timeout(Duration::from_millis(5)) {
                            Ok((request, reply)) => {
                                let _ = reply.send(state.request(request));
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                    state.disconnect();
                })
                .map_err(|e| e.to_string())?;
            *worker = Some(Worker { tx, join });
        }
        let (tx, rx) = mpsc::channel();
        worker
            .as_ref()
            .unwrap()
            .tx
            .send((request, tx))
            .map_err(|_| "CAN worker stopped")?;
        drop(worker);
        rx.recv().map_err(|_| "CAN worker stopped")?
    }
}
impl Drop for CanService {
    fn drop(&mut self) {
        if let Ok(slot) = self.worker.get_mut() {
            if let Some(Worker { tx, join }) = slot.take() {
                drop(tx);
                let _ = join.join();
            }
        }
    }
}
impl crate::StudioApp {
    /// Override the child executable before making any CAN requests (e.g. in tests).
    pub fn with_can_worker_executable(self, path: PathBuf) -> Self {
        *self
            .can
            .worker_executable
            .lock()
            .expect("CAN worker executable lock poisoned") = Some(path);
        self
    }

    pub fn set_can_library(&self, path: PathBuf) {
        *self.can.library.lock().expect("CAN library lock poisoned") = Some(path);
    }
    pub fn can_request(&self, mut request: Request) -> Result<Snapshot, String> {
        if let Request::Scan { library } = &mut request {
            if library.is_none() {
                *library = self
                    .can
                    .library
                    .lock()
                    .map_err(|_| "CAN library lock poisoned")?
                    .clone();
            }
        }
        self.can.request(request)
    }
}

struct Recording {
    path: String,
    output: Output,
    count: u64,
}
enum Output {
    Csv(BufWriter<File>),
    Mcap(Box<mcap::Writer<BufWriter<File>>>, u16),
}
impl Recording {
    fn create(path: PathBuf, configs: &[ChannelConfig]) -> Result<Self, String> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext != "csv" && ext != "mcap" {
            return Err("Choose a .mcap or .csv capture file".into());
        }
        // Never silently replace a capture. The UI suggests a fresh timestamped name.
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("Cannot create capture: {e}"))?;
        let output = if ext == "csv" {
            let mut w = BufWriter::new(file);
            writeln!(w,"sequence,host_timestamp_ns,device_timestamp_raw,channel,direction,id,extended,fd,brs,rtr,esi,ack,dlc,data").map_err(|e|e.to_string())?;
            Output::Csv(w)
        } else {
            let mut w = mcap::WriteOptions::new()
                .compression(None)
                .create(BufWriter::new(file))
                .map_err(|e| e.to_string())?;
            let schema=w.add_schema("tuning_studio.CanFrame","jsonschema",br#"{"type":"object","description":"Raw CAN frame. deviceTimestamp is an opaque SDK clock; hostTimestampNs is Unix nanoseconds. Timestamps are decimal strings."}"#).map_err(|e|e.to_string())?;
            let meta = BTreeMap::from([(
                "channel_configuration".into(),
                serde_json::to_string(configs).map_err(|e| e.to_string())?,
            )]);
            let channel = w
                .add_channel(schema, "can/frames", "json", &meta)
                .map_err(|e| e.to_string())?;
            Output::Mcap(Box::new(w), channel)
        };
        Ok(Self {
            path: path.to_string_lossy().into_owned(),
            output,
            count: 0,
        })
    }
    fn write(&mut self, f: &Frame) -> Result<(), String> {
        match &mut self.output {
            Output::Csv(w) => {
                let hex = f
                    .data
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                writeln!(
                    w,
                    "{},{},{},{},{},{:X},{},{},{},{},{},{},{},{}",
                    f.sequence,
                    f.host_timestamp_ns,
                    f.device_timestamp,
                    f.channel,
                    f.direction,
                    f.id,
                    f.extended,
                    f.fd,
                    f.brs,
                    f.rtr,
                    f.esi,
                    f.ack,
                    f.dlc,
                    hex
                )
                .map_err(|e| e.to_string())?;
            }
            Output::Mcap(w, ch) => {
                let at = f
                    .host_timestamp_ns
                    .parse()
                    .map_err(|_| "Invalid host timestamp")?;
                let header = mcap::records::MessageHeader {
                    channel_id: *ch,
                    sequence: self.count as u32,
                    log_time: at,
                    publish_time: at,
                };
                w.write_to_known_channel(
                    &header,
                    &serde_json::to_vec(f).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
            }
        }
        self.count += 1;
        Ok(())
    }
    fn finish(mut self) -> Result<(), String> {
        match &mut self.output {
            Output::Csv(w) => w.flush().map_err(|e| e.to_string()),
            Output::Mcap(w, _) => w.finish().map(|_| ()).map_err(|e| e.to_string()),
        }
    }
}
const HISTORY: usize = 10000;
#[derive(Default)]
struct Capture {
    adapter: Option<RemoteAdapter>,
    library: Option<PathBuf>,
    worker_executable: Option<PathBuf>,
    state: Snapshot,
    history: VecDeque<Frame>,
    recording: Option<Recording>,
}
impl Capture {
    fn drain(&mut self) {
        let Some(adapter) = &mut self.adapter else {
            return;
        };
        let frames = adapter.drain();
        self.state.dropped = adapter.dropped();
        if self.state.connected {
            if let Some(error) = adapter.error() {
                self.state.error = Some(error.into());
                self.state.connected = false;
                let _ = self.finish_recording();
            }
        }
        for mut f in frames {
            self.state.sequence += 1;
            f.sequence = self.state.sequence;
            match f.direction.as_str() {
                "rx" => self.state.received += 1,
                "tx" => self.state.transmitted += 1,
                _ => self.state.errors += 1,
            }
            if let Some(r) = &mut self.recording {
                match r.write(&f) {
                    Ok(()) => self.state.recorded = r.count,
                    Err(e) => {
                        self.state.error = Some(format!("CAN recording failed: {e}"));
                        let _ = self.finish_recording();
                    }
                }
            }
            if self.history.len() == HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(f);
        }
    }
    fn finish_recording(&mut self) -> Result<(), String> {
        self.state.recording = None;
        if let Some(r) = self.recording.take() {
            r.finish()?;
        }
        Ok(())
    }
    fn disconnect(&mut self) {
        self.state.connected = false;
        if let Some(a) = &mut self.adapter {
            a.stop();
            if let Some(error) = a.error() {
                self.state.error = Some(error.into());
            }
        }
        // Close stops callbacks; drain all pending frames before finalizing the file.
        self.drain();
        // drain() is bounded; continue until no new frames were consumed.
        loop {
            let n = self.state.sequence;
            self.drain();
            if self.state.sequence == n {
                break;
            }
        }
        if let Err(e) = self.finish_recording() {
            self.state.error = Some(e);
        }
        self.adapter = None;
    }
    fn request(&mut self, r: Request) -> Result<Snapshot, String> {
        let mut after = None;
        match r {
            Request::Scan { library } => {
                if self.state.connected {
                    return Err("Disconnect CAN before scanning".into());
                }
                if let Some(mut a) = self.adapter.take() {
                    a.stop();
                }
                self.state.devices.clear();
                self.library = library;
                let (a, devices) = RemoteAdapter::load(
                    self.library.as_deref(),
                    self.worker_executable.as_deref(),
                )?;
                self.state.devices = devices;
                self.adapter = Some(a);
                self.state.error = None;
            }
            Request::Connect {
                index,
                channels,
                configs,
            } => {
                if self.state.connected {
                    return Err("CAN is already connected".into());
                }
                if self.state.devices.is_empty() {
                    return Err("Scan for adapters first".into());
                }
                if self.adapter.is_none() {
                    let (a, devices) = RemoteAdapter::load(
                        self.library.as_deref(),
                        self.worker_executable.as_deref(),
                    )?;
                    if devices.len() != self.state.devices.len() {
                        return Err("Adapters changed; scan again".into());
                    }
                    self.adapter = Some(a);
                }
                self.adapter
                    .as_mut()
                    .ok_or("Scan for adapters first")?
                    .start(index, channels, configs.clone())?;
                self.history.clear();
                self.state.received = 0;
                self.state.transmitted = 0;
                self.state.errors = 0;
                self.state.dropped = 0;
                self.state.connected = true;
                self.state.configs = configs;
                self.state.error = None;
            }
            Request::Disconnect => self.disconnect(),
            Request::Send { frame } => {
                self.adapter
                    .as_mut()
                    .ok_or("CAN is disconnected")?
                    .send(&frame)?;
            }
            Request::Poll { after: cursor } => after = Some(cursor),
            Request::Record { path } => {
                if !self.state.connected {
                    return Err("Connect CAN before recording".into());
                }
                if self.recording.is_some() {
                    return Err("Stop the current CAN recording first".into());
                }
                let r = Recording::create(path, &self.state.configs)?;
                self.state.recording = Some(r.path.clone());
                self.state.recorded = 0;
                self.state.error = None;
                self.recording = Some(r);
            }
            Request::StopRecording => {
                self.drain();
                self.finish_recording()?;
            }
        }
        let mut snapshot = self.state.clone();
        if let Some(after) = after {
            snapshot.skipped = self
                .history
                .front()
                .map_or(0, |f| f.sequence.saturating_sub(after.saturating_add(1)));
            snapshot.frames = self
                .history
                .iter()
                .filter(|f| f.sequence > after)
                .take(2000)
                .cloned()
                .collect();
            snapshot.sequence = snapshot
                .frames
                .last()
                .map_or(self.state.sequence, |f| f.sequence);
        }
        Ok(snapshot)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_does_not_overwrite_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.csv");
        std::fs::write(&path, "keep").unwrap();
        assert!(Recording::create(path.clone(), &[]).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "keep");
    }
    #[test]
    fn history_reports_missing_frames() {
        let mut c = Capture::default();
        for sequence in 101..=2200 {
            c.history.push_back(Frame {
                sequence,
                device_timestamp: "0".into(),
                host_timestamp_ns: "0".into(),
                channel: 0,
                id: 1,
                extended: false,
                fd: false,
                brs: false,
                rtr: false,
                esi: false,
                ack: false,
                dlc: 0,
                direction: "rx".into(),
                data: vec![],
            });
        }
        c.state.sequence = 2200;
        let first = c.request(Request::Poll { after: 0 }).unwrap();
        assert_eq!(first.skipped, 100);
        assert_eq!(first.frames.len(), 2000);
        assert_eq!(first.sequence, 2100);
        let second = c
            .request(Request::Poll {
                after: first.sequence,
            })
            .unwrap();
        assert_eq!(second.skipped, 0);
        assert_eq!(second.frames.len(), 100);
        assert_eq!(second.sequence, 2200);
        let idle = c
            .request(Request::Poll {
                after: second.sequence,
            })
            .unwrap();
        assert!(idle.frames.is_empty());
        assert_eq!(idle.sequence, 2200);
    }
}
