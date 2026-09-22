//! Everything the UI asks of the backend, as plain blocking methods on [`StudioApp`].
//!
//! The desktop app wraps these in Tauri commands; `studio-server` serves them over
//! stdio to the VS Code extension. Session output (sample frames and events) goes to
//! the [`SessionSink`](studio_core::SessionSink) passed to [`StudioApp::connect`].
//!
//! Every session's output also passes through a [`Tap`] ahead of that sink, so
//! recording ([`record`]) and the TCP stream ([`stream`]) see every tick whether or
//! not the UI keeps up. Their state reaches the UI as [`AppEvent`]s.

pub mod can;
pub mod elf;
pub mod record;
pub mod session;
pub mod stream;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;

pub use elf::{startup_elf_path, OpenedElf};
pub use record::{export_csv, CsvExport, RecordingState};
pub use session::{
    list_probes, list_serial_ports, search_chips, Carrier, ConnectRequest, ProbeOpener,
    TaskSnapshot, ValueRead, WatchRequest, WatchResult,
};
pub use stream::{StreamOptions, StreamState, DEFAULT_PORT};
pub use studio_core::{SessionEvent, SessionSink};

use elf::LoadedElf;
use session::SessionState;
use studio_core::Tap;

/// The app's version, written into recordings and the stream's hello
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// State changes that belong to the app rather than to one session.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AppEvent {
    Recording(RecordingState),
    Stream(StreamState),
}

/// Where [`AppEvent`]s go; set once by the host.
pub type AppEventSink = Arc<dyn Fn(AppEvent) + Send + Sync>;

/// The recording and stream state, for a page that starts after they did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    pub recording: Option<RecordingState>,
    pub stream: StreamState,
}

/// The loaded ELF and the running session, shared by every request.
pub struct StudioApp {
    can: can::CanService,
    elf: LoadedElf,
    session: SessionState,
    probe_opener: ProbeOpener,
    /// Read the defmt log and serve tuning over the firmware's RTT channels
    rtt: bool,
    tap: Arc<Tap>,
    recorder: Mutex<Option<record::Recorder>>,
    stream: Mutex<Option<stream::StreamServer>>,
    events: Mutex<Option<AppEventSink>>,
    recordings_dir: Mutex<Option<PathBuf>>,
}

impl Default for StudioApp {
    fn default() -> Self {
        Self::new()
    }
}

impl StudioApp {
    /// Connects to real probes.
    pub fn new() -> Self {
        Self::with_probe_opener(session::real_probe())
    }

    /// Opens the probe carrier with `opener` instead, e.g. a mock target for tests.
    pub fn with_probe_opener(opener: ProbeOpener) -> Self {
        Self {
            can: can::CanService::default(),
            elf: LoadedElf::default(),
            session: SessionState::default(),
            probe_opener: opener,
            rtt: true,
            tap: Tap::new(),
            recorder: Mutex::new(None),
            stream: Mutex::new(None),
            events: Mutex::new(None),
            recordings_dir: Mutex::new(None),
        }
    }

    /// How this app opens the probe carrier
    pub fn probe_opener(&self) -> ProbeOpener {
        self.probe_opener.clone()
    }

    /// Leave the firmware's RTT channels alone: no log, no tuning over RTT. Reading
    /// the log moves the channel's read offset, which is a write to target memory.
    pub fn without_rtt(mut self) -> Self {
        self.rtt = false;
        self
    }

    /// Where recording and stream state changes go.
    pub fn set_event_sink(&self, sink: AppEventSink) {
        *self.events.lock().expect("event sink poisoned") = Some(sink);
    }

    /// Where a recording goes when no path is given.
    pub fn set_recordings_dir(&self, dir: PathBuf) {
        *self.recordings_dir.lock().expect("recordings dir poisoned") = Some(dir);
    }

    /// Every session's samples and events, for subscribers of its own.
    pub fn tap(&self) -> &Arc<Tap> {
        &self.tap
    }

    fn event_sink(&self) -> Option<AppEventSink> {
        self.events.lock().expect("event sink poisoned").clone()
    }

    pub fn app_state(&self) -> AppState {
        AppState {
            recording: self
                .recorder
                .lock()
                .expect("recorder poisoned")
                .as_ref()
                .map(|r| r.state()),
            stream: self
                .stream
                .lock()
                .expect("stream poisoned")
                .as_ref()
                .map_or_else(StreamState::stopped, |s| s.state()),
        }
    }
}
