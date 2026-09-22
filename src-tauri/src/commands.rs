//! Tauri commands: thin wrappers over [`StudioApp`]. Commands that block (disk,
//! DWARF parsing, joining a session, waiting on the target) run on a blocking
//! thread; the rest run inline as before. Samples reach the webview as binary
//! frames on one channel, status, stats and log lines as JSON on another.
//! Recording and stream state goes to every window as the `studio-app-event` event.

use std::path::PathBuf;
use std::sync::Arc;

use studio_app::{
    AppState, ConnectRequest, CsvExport, OpenedElf, RecordingState, SessionEvent, SessionSink,
    StreamOptions, StreamState, StudioApp, TaskSnapshot, ValueRead, WatchRequest, WatchResult,
};
use studio_carriers::probe::ProbeInfo;
use studio_carriers::serial::PortInfo;
use studio_dwarf::tree::Children;
use studio_dwarf::{NodeRef, SymbolNode};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

pub type App = Arc<StudioApp>;

struct ChannelSink {
    data: Channel<InvokeResponseBody>,
    events: Channel<SessionEvent>,
}

impl SessionSink for ChannelSink {
    fn frame(&self, bytes: Vec<u8>) -> bool {
        self.data.send(InvokeResponseBody::Raw(bytes)).is_ok()
    }

    fn event(&self, event: SessionEvent) {
        let _ = self.events.send(event);
    }
}

async fn blocking<T: Send + 'static>(
    body: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(body)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn open_elf(path: PathBuf, app: State<'_, App>) -> Result<OpenedElf, String> {
    let app = app.inner().clone();
    blocking(move || app.open_elf(path)).await
}

#[tauri::command]
pub fn symbol_children(
    node: NodeRef,
    limit: Option<usize>,
    app: State<'_, App>,
) -> Result<Children, String> {
    app.symbol_children(&node, limit)
}

/// ELF to open at startup, from `TUNING_TOOLS_ELF` (development convenience).
#[tauri::command]
pub fn startup_elf_path() -> Option<String> {
    studio_app::startup_elf_path()
}

#[tauri::command]
pub async fn list_probes() -> Result<Vec<ProbeInfo>, String> {
    blocking(|| Ok(studio_app::list_probes())).await
}

#[tauri::command]
pub async fn search_chips(query: String) -> Result<Vec<String>, String> {
    blocking(move || Ok(studio_app::search_chips(&query))).await
}

#[tauri::command]
pub async fn list_serial_ports() -> Result<Vec<PortInfo>, String> {
    blocking(|| Ok(studio_app::list_serial_ports())).await
}

#[tauri::command]
pub async fn session_connect(
    request: ConnectRequest,
    data: Channel<InvokeResponseBody>,
    events: Channel<SessionEvent>,
    app: State<'_, App>,
) -> Result<(), String> {
    let app = app.inner().clone();
    blocking(move || app.connect(request, Arc::new(ChannelSink { data, events }))).await
}

#[tauri::command]
pub async fn session_disconnect(app: State<'_, App>) -> Result<(), String> {
    let app = app.inner().clone();
    blocking(move || {
        app.disconnect();
        Ok(())
    })
    .await
}

/// Resolve watches and hand the sampleable ones to the session.
#[tauri::command]
pub fn session_set_watches(
    watches: Vec<WatchRequest>,
    app: State<'_, App>,
) -> Result<Vec<WatchResult>, String> {
    app.set_watches(watches)
}

/// Ask the firmware to go back to every tunable's built-in default.
#[tauri::command]
pub async fn session_discard(app: State<'_, App>) -> Result<(), String> {
    let app = app.inner().clone();
    blocking(move || app.discard()).await
}

/// Ask the firmware to keep every current value across a power cycle.
#[tauri::command]
pub async fn session_save(app: State<'_, App>) -> Result<(), String> {
    let app = app.inner().clone();
    blocking(move || app.save()).await
}

#[tauri::command]
pub fn session_set_rate(hz: f64, app: State<'_, App>) {
    app.set_rate(hz);
}

/// Ask the firmware to run tuning value `id` at `value`.
#[tauri::command]
pub async fn session_request(id: u32, value: f64, app: State<'_, App>) -> Result<(), String> {
    let app = app.inner().clone();
    blocking(move || app.request(id, value)).await
}

/// Numeric leaves under `node` (the node itself when it is one), depth first.
#[tauri::command]
pub fn watchable_leaves(node: NodeRef, app: State<'_, App>) -> Result<Vec<SymbolNode>, String> {
    app.watchable_leaves(&node)
}

/// Read every embassy task's state, and the firmware's task counters when it
/// keeps them, in one pass over target memory.
#[tauri::command]
pub async fn session_task_states(app: State<'_, App>) -> Result<TaskSnapshot, String> {
    let app = app.inner().clone();
    blocking(move || app.task_states()).await
}

/// Read each numeric node once, outside the sampled watch set.
#[tauri::command]
pub async fn session_read_values(
    nodes: Vec<NodeRef>,
    app: State<'_, App>,
) -> Result<Vec<ValueRead>, String> {
    let app = app.inner().clone();
    blocking(move || app.read_values(&nodes)).await
}

/// Start recording the session: to `path`, or to a new file in the app's
/// `recordings` directory.
#[tauri::command]
pub async fn recording_start(
    path: Option<PathBuf>,
    app: State<'_, App>,
) -> Result<RecordingState, String> {
    let app = app.inner().clone();
    blocking(move || app.start_recording(path, None)).await
}

#[tauri::command]
pub async fn recording_stop(app: State<'_, App>) -> Result<RecordingState, String> {
    let app = app.inner().clone();
    blocking(move || app.stop_recording()).await
}

#[tauri::command]
pub async fn export_csv(
    mcap_path: PathBuf,
    csv_path: Option<PathBuf>,
) -> Result<CsvExport, String> {
    blocking(move || studio_app::export_csv(&mcap_path, csv_path.as_deref())).await
}

#[tauri::command]
pub async fn stream_start(
    port: Option<u16>,
    bind_all: Option<bool>,
    app: State<'_, App>,
) -> Result<StreamState, String> {
    let app = app.inner().clone();
    let options = StreamOptions::new(
        port.unwrap_or(studio_app::DEFAULT_PORT),
        bind_all.unwrap_or(false),
    );
    blocking(move || app.start_stream(options)).await
}

#[tauri::command]
pub async fn stream_stop(app: State<'_, App>) -> Result<StreamState, String> {
    let app = app.inner().clone();
    blocking(move || Ok(app.stop_stream())).await
}

#[tauri::command]
pub fn app_state(app: State<'_, App>) -> AppState {
    app.app_state()
}

#[tauri::command]
pub async fn session_task_trace(
    app: State<'_, App>,
) -> Result<Option<studio_dwarf::task_trace::TraceSnapshot>, String> {
    let app = app.inner().clone();
    blocking(move || app.task_trace()).await
}

#[tauri::command]
pub async fn inspect_children(
    node: NodeRef,
    offset: u64,
    limit: usize,
    live: bool,
    app: State<'_, App>,
) -> Result<Children, String> {
    let app = app.inner().clone();
    blocking(move || app.inspect_children(&node, offset, limit, live)).await
}
#[tauri::command]
pub fn node_metadata(node: NodeRef, app: State<'_, App>) -> Result<SymbolNode, String> {
    app.node_metadata(&node)
}

#[tauri::command]
pub async fn can_request(
    request: studio_app::can::Request,
    app: State<'_, App>,
) -> Result<studio_app::can::Snapshot, String> {
    let app = app.inner().clone();
    blocking(move || app.can_request(request)).await
}
