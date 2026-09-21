//! Session control. The session thread (studio-core) owns the probe or the
//! serial port; these methods only start, stop and steer it. Its output goes to
//! the [`SessionSink`] handed to [`StudioApp::connect`].
//!
//! Every method may block (joining a stopped session, waiting for the target), so
//! callers on an async runtime run them on a blocking thread.

use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use studio_carriers::probe::{self, ProbeConfig, ProbeInfo, ProbeLink};
use studio_carriers::serial::{self, PortInfo, SerialStream};
use studio_carriers::{ByteStream, CarrierError, Link};
use studio_core::catalog::{Catalog, TableLayout};
use studio_core::link::{spawn_link, LinkOptions};
use studio_core::plan::{decode, scalar_len, ReadPlan};
use studio_core::session::SessionOptions;
use studio_core::tap::{SessionInfo, TunableName, TuneRequest};
use studio_core::{ReadItem, Session, SessionCommand, SessionSink, TapSink, WatchMeta};
use studio_dwarf::task_stats::TaskCounters;
use studio_dwarf::tasks::TaskState;
use studio_dwarf::tree::{self, NodeKind};
use studio_dwarf::{ElfInfo, NodeRef, SymbolNode};

use crate::elf::Tuning;
use crate::{StudioApp, APP_VERSION};

/// Scalars collected by [`StudioApp::watchable_leaves`] before it stops.
const MAX_LEAVES: usize = 256;
/// Longest a tuning write waits for the session thread
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// Opens the probe carrier for a connect: the real probe, or a stand-in for tests.
/// Gets the probe settings and the path of the open ELF.
pub type ProbeOpener =
    Arc<dyn Fn(&ProbeConfig, &Path) -> Result<Box<dyn Link>, CarrierError> + Send + Sync>;

pub(crate) fn real_probe() -> ProbeOpener {
    Arc::new(|config, _| ProbeLink::open(config).map(|link| Box::new(link) as Box<dyn Link>))
}

#[derive(Default)]
pub(crate) struct SessionState {
    session: Mutex<Option<Session>>,
    /// Last watched set, re-sent when a new session connects
    watches: Mutex<Vec<ReadItem>>,
    /// Tuning values watched by id, as `(watch id, value id)`, for a framed link
    cell_watches: Mutex<Vec<(u32, u32)>>,
}

impl SessionState {
    /// Hand a newly opened ELF's tuning table to a running session.
    pub fn set_tuning(&self, tuning: Option<Tuning>) {
        if let Some(session) = self
            .session
            .lock()
            .expect("session state poisoned")
            .as_ref()
        {
            session.send(catalog_command(tuning));
        }
    }

    fn send(&self, command: SessionCommand) -> bool {
        self.session
            .lock()
            .expect("session state poisoned")
            .as_ref()
            .is_some_and(|s| s.send(command))
    }

    fn take(&self) -> Option<Session> {
        self.session.lock().expect("session state poisoned").take()
    }
}

fn catalog_command(tuning: Option<Tuning>) -> SessionCommand {
    SessionCommand::SetCatalog(tuning.map(|t| Box::new((*t).clone())))
}

pub fn list_probes() -> Vec<ProbeInfo> {
    probe::list_probes()
}

/// Chip names containing `query`, case-insensitive, at most 50
pub fn search_chips(query: &str) -> Vec<String> {
    static ALL: OnceLock<Vec<String>> = OnceLock::new();
    let all = ALL.get_or_init(|| probe::search_chips("", usize::MAX));
    let needle = query.to_ascii_lowercase();
    all.iter()
        .filter(|name| name.to_ascii_lowercase().contains(&needle))
        .take(50)
        .cloned()
        .collect()
}

pub fn list_serial_ports() -> Vec<PortInfo> {
    serial::list_ports()
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Carrier {
    /// A debug probe on SWD: needs the ELF and the chip
    Probe,
    /// The firmware's framed link on a serial port: needs neither
    Serial,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ConnectRequest {
    pub carrier: Carrier,
    pub probe: Option<String>,
    pub chip: String,
    pub speed_khz: Option<u32>,
    pub port: Option<String>,
    pub rate_hz: f64,
}

/// A value to sample: a symbol path, or a tuning table value by id. The rest
/// describes it to recordings and the stream; the backend fills in what is missing.
#[derive(Deserialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WatchRequest {
    pub id: u32,
    pub node: Option<NodeRef>,
    pub cell: Option<u32>,
    /// Display name, e.g. `GIMBAL.yaw.angle`
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
    /// Symbol path, or the tuning value's name
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub type_name: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchResult {
    pub id: u32,
    /// Why the value cannot be sampled; `None` when it is
    pub error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    /// The task slot's node path, as `Task.root.path`
    path: String,
    state: Option<TaskState>,
    /// Why the state could not be read
    error: Option<String>,
    /// The firmware's counters for this task, when it keeps them
    counters: Option<TaskCounters>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    tasks: Vec<TaskStatus>,
    /// Host time of the read, in microseconds from an arbitrary start
    host_us: u64,
    /// Whether the firmware links rm-task-stats
    has_stats: bool,
    /// Cycle counter frequency, when the counters were read
    clock_hz: Option<u32>,
    /// Tasks spawned while every counter slot was taken
    untracked: u32,
    /// Why the counters could not be read
    stats_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueRead {
    /// `None` when it could not be read
    value: Option<f64>,
    text: Option<String>,
    error: Option<String>,
}

impl StudioApp {
    /// Start a session, replacing any running one. Returns once it is spawned;
    /// the link's state and samples arrive through `sink`.
    pub fn connect(
        &self,
        request: ConnectRequest,
        sink: Arc<dyn SessionSink>,
    ) -> Result<(), String> {
        if request.carrier == Carrier::Serial {
            return self.connect_serial(request, sink);
        }
        let elf = self.elf.current()?;
        if request.chip.trim().is_empty() {
            return Err("choose the target chip first".into());
        }
        // Stopping joins the old thread and the probe detaches
        drop(self.session.take());
        let elf_bytes =
            std::fs::read(&elf.path).map_err(|e| format!("could not read {}: {e}", elf.path))?;

        let config = ProbeConfig {
            selector: request.probe.filter(|s| !s.is_empty()),
            chip: request.chip.trim().to_string(),
            speed_khz: request.speed_khz,
        };
        let rtt_address = self
            .rtt
            .then(|| elf.find_symbol("_SEGGER_RTT").map(|s| s.address))
            .flatten();
        let tuning = self.elf.tuning()?;
        let open = self.probe_opener.clone();
        let elf_path = elf.path.clone();
        self.tap.connecting(SessionInfo {
            elf: Path::new(&elf.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
            elf_path: Some(elf.path.clone()),
            build_id: build_id(&elf_bytes),
            chip: Some(config.chip.clone()),
            carrier: "probe".into(),
            port: None,
            rate_hz: request.rate_hz,
            app_version: APP_VERSION.into(),
            tunables: tuning
                .as_ref()
                .map_or_else(Vec::new, |t| tunable_names(&t.1)),
        });
        let sink: Arc<dyn SessionSink> = Arc::new(TapSink::new(self.tap.clone(), sink));
        let session = Session::spawn(
            move || open(&config, Path::new(&elf_path)),
            SessionOptions {
                rate_hz: request.rate_hz,
                elf: Some(elf_bytes),
                rtt_address,
            },
            sink,
        );
        let watches = self
            .session
            .watches
            .lock()
            .expect("session state poisoned")
            .clone();
        if tuning.is_some() {
            session.send(catalog_command(tuning));
        }
        if !watches.is_empty() {
            session.send(SessionCommand::SetWatches(watches));
        }
        *self.session.session.lock().expect("session state poisoned") = Some(session);
        Ok(())
    }

    fn connect_serial(
        &self,
        request: ConnectRequest,
        sink: Arc<dyn SessionSink>,
    ) -> Result<(), String> {
        let port = request
            .port
            .filter(|p| !p.is_empty())
            .ok_or("choose the serial port first")?;
        drop(self.session.take());
        let elf = self.elf.current().ok();
        self.tap.connecting(SessionInfo {
            elf: elf.as_ref().and_then(|e| {
                Path::new(&e.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            }),
            elf_path: elf.as_ref().map(|e| e.path.clone()),
            build_id: None,
            chip: None,
            carrier: "serial".into(),
            port: Some(port.clone()),
            rate_hz: request.rate_hz,
            app_version: APP_VERSION.into(),
            tunables: Vec::new(),
        });
        let sink: Arc<dyn SessionSink> = Arc::new(TapSink::new(self.tap.clone(), sink));
        let session = spawn_link(
            move || SerialStream::open(&port, 115_200).map(|s| Box::new(s) as Box<dyn ByteStream>),
            LinkOptions {
                rate_hz: request.rate_hz,
            },
            sink,
        );
        let watches = self
            .session
            .cell_watches
            .lock()
            .expect("session state poisoned")
            .clone();
        if !watches.is_empty() {
            session.send(SessionCommand::SetCellWatches(watches));
        }
        *self.session.session.lock().expect("session state poisoned") = Some(session);
        Ok(())
    }

    /// Stop the session and wait for it to let go of the probe or port. A
    /// recording ends with it, after the session's last samples.
    pub fn disconnect(&self) {
        drop(self.session.take());
        self.finish_recording();
    }

    /// Resolve watches and hand the sampleable ones to the session. Symbols
    /// resolve against the loaded ELF for a probe; tuning values also go by id to
    /// a framed link, which needs no ELF.
    pub fn set_watches(&self, watches: Vec<WatchRequest>) -> Result<Vec<WatchResult>, String> {
        let state = &self.session;
        let cells: Vec<(u32, u32)> = watches
            .iter()
            .filter_map(|w| w.cell.map(|cell| (w.id, cell)))
            .collect();
        // Only a change of what is sampled goes to the session: a new unit or
        // name must not restart sampling
        let mut old_cells = state.cell_watches.lock().expect("session state poisoned");
        if *old_cells != cells {
            state.send(SessionCommand::SetCellWatches(cells.clone()));
            *old_cells = cells;
        }
        drop(old_cells);

        let loaded = self
            .elf
            .current()
            .ok()
            .map(|info| (info, self.elf.tuning().ok().flatten()));
        let mut items = Vec::new();
        let mut metas = Vec::new();
        let results = watches
            .into_iter()
            .map(|w| {
                let resolved = match &loaded {
                    Some((info, tuning)) => resolve_watch(info, tuning.as_deref(), &w).map(Some),
                    None if w.cell.is_some() => Ok(None),
                    None => Err("open the ELF to watch symbols".to_string()),
                };
                let error = match resolved {
                    Ok(Some(mut item)) => {
                        item.id = w.id;
                        items.push(item);
                        None
                    }
                    Ok(None) => None,
                    Err(error) => Some(error),
                };
                if error.is_none() {
                    metas.push(watch_meta(loaded.as_ref().map(|l| &*l.0), &w));
                }
                WatchResult { id: w.id, error }
            })
            .collect();
        let mut old_items = state.watches.lock().expect("session state poisoned");
        if *old_items != items {
            state.send(SessionCommand::SetWatches(items.clone()));
            *old_items = items;
        }
        drop(old_items);
        self.tap.set_watches(metas);
        Ok(results)
    }

    /// Ask the firmware to go back to every tunable's built-in default.
    pub fn discard(&self) -> Result<(), String> {
        let (reply, rx) = mpsc::sync_channel(1);
        if !self.session.send(SessionCommand::Discard { reply }) {
            return Err("connect to the target first".into());
        }
        let result = wait_reply_for(rx, REQUEST_TIMEOUT);
        self.note_request("discard", None, None, &result);
        result
    }

    /// Ask the firmware to keep every current value across a power cycle.
    pub fn save(&self) -> Result<(), String> {
        let (reply, rx) = mpsc::sync_channel(1);
        if !self.session.send(SessionCommand::Save { reply }) {
            return Err("connect to the target first".into());
        }
        let result = wait_reply_for(rx, studio_core::link::SAVE_TIMEOUT + REQUEST_TIMEOUT);
        self.note_request("save", None, None, &result);
        result
    }

    pub fn set_rate(&self, hz: f64) {
        if self.session.send(SessionCommand::SetRate(hz)) {
            self.tap.set_rate(hz);
        }
    }

    /// Ask the firmware to run tuning value `id` at `value`.
    pub fn request(&self, id: u32, value: f64) -> Result<(), String> {
        let (reply, rx) = mpsc::sync_channel(1);
        if !self
            .session
            .send(SessionCommand::Request { id, value, reply })
        {
            return Err("connect to the target first".into());
        }
        let result = wait_reply_for(rx, REQUEST_TIMEOUT);
        self.note_request("set", Some(id), Some(value), &result);
        result
    }

    /// Tell recordings and the stream about a tuning request and how it went.
    fn note_request(
        &self,
        action: &'static str,
        id: Option<u32>,
        value: Option<f64>,
        result: &Result<(), String>,
    ) {
        self.tap.tune_request(TuneRequest {
            action,
            id,
            value,
            error: result.as_ref().err().cloned(),
        });
    }

    /// Numeric leaves under `node` (the node itself when it is one), depth first.
    pub fn watchable_leaves(&self, node: &NodeRef) -> Result<Vec<SymbolNode>, String> {
        let elf = self.elf.current()?;
        let start = tree::node(&elf, node).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        collect_leaves(&elf, start, &mut out)?;
        Ok(out)
    }

    /// Read every embassy task's state, and the firmware's task counters when it
    /// keeps them, in one pass over target memory.
    pub fn task_states(&self) -> Result<TaskSnapshot, String> {
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        let (probes, stats) = self.elf.task_probes()?;
        let mut regions: Vec<(u64, usize)> = probes
            .iter()
            .filter_map(|(_, _, p)| p.as_ref().ok())
            .flat_map(|p| p.regions())
            .collect();
        let has_stats = stats.is_some();
        let layout = match &stats {
            Some(Ok(layout)) => {
                regions.push(layout.region());
                Some(layout.clone())
            }
            _ => None,
        };
        let bytes = self.read_once(regions)?;
        let host_us = EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64;
        let mut bytes = bytes.into_iter();

        let tasks: Vec<(String, u64, Option<TaskState>, Option<String>)> = probes
            .iter()
            .map(|(path, address, probe)| match probe {
                Ok(probe) => {
                    let mine: Vec<Vec<u8>> = bytes.by_ref().take(probe.regions().len()).collect();
                    let state = probe.decode(&mine);
                    let error = state.is_none().then(|| "short read".to_string());
                    (path.clone(), *address, state, error)
                }
                Err(error) => (path.clone(), *address, None, Some(error.clone())),
            })
            .collect();

        let reading = layout.map(|layout| {
            bytes
                .next()
                .ok_or_else(|| "short read".to_string())
                .and_then(|b| layout.decode(&b))
        });
        let (reading, stats_error) = match (reading, stats) {
            (Some(Ok(r)), _) => (Some(r), None),
            (Some(Err(e)), _) | (None, Some(Err(e))) => (None, Some(e)),
            (None, _) => (None, None),
        };
        Ok(TaskSnapshot {
            tasks: tasks
                .into_iter()
                .map(|(path, address, state, error)| TaskStatus {
                    counters: reading.as_ref().and_then(|r| r.task(address).copied()),
                    path,
                    state,
                    error,
                })
                .collect(),
            host_us,
            has_stats,
            clock_hz: reading.as_ref().map(|r| r.clock_hz),
            untracked: reading.as_ref().map_or(0, |r| r.untracked),
            stats_error,
        })
    }

    /// Read each numeric node once, outside the sampled watch set.
    pub fn read_values(&self, nodes: &[NodeRef]) -> Result<Vec<ValueRead>, String> {
        if nodes.len() > MAX_LEAVES {
            return Err(format!("read at most {MAX_LEAVES} values per request"));
        }
        let elf = self.elf.current()?;
        let mut pointers = std::collections::HashMap::new();
        let mut read_pointer = |address: u64, len: usize| -> Result<u64, String> {
            pointers
                .entry((address, len))
                .or_insert_with(|| {
                    let bytes = self.read_once(vec![(address, len)])?;
                    let b = bytes
                        .first()
                        .filter(|b| b.len() == len)
                        .ok_or("short pointer read")?;
                    let mut raw = [0u8; 8];
                    if elf.is_little_endian {
                        raw[..len].copy_from_slice(b);
                    } else {
                        raw[8 - len..].copy_from_slice(b);
                    }
                    let value = if elf.is_little_endian {
                        u64::from_le_bytes(raw)
                    } else {
                        u64::from_be_bytes(raw)
                    };
                    if value == 0 {
                        Err("null pointer".into())
                    } else {
                        Ok(value)
                    }
                })
                .clone()
        };
        let items: Vec<Result<ReadItem, String>> = nodes
            .iter()
            .map(|n| {
                tree::node_with_memory(&elf, n, &mut read_pointer)
                    .map_err(|e| e.to_string())
                    .and_then(read_item)
                    .and_then(|item| {
                        item.span()
                            .map(|_| item)
                            .ok_or_else(|| "not a single number".to_string())
                    })
            })
            .collect();
        // Use the same datavis-rs coalescer as plot sampling. One command goes
        // through the session owner; neighbouring inspector values share a read.
        let plan = ReadPlan::new(
            items
                .iter()
                .filter_map(|item| item.as_ref().ok().cloned())
                .collect(),
        );
        let regions = plan.regions();
        let bytes = match self.read_once(
            regions
                .iter()
                .map(|r| (r.address, r.len as usize))
                .collect(),
        ) {
            Ok(bytes) => bytes,
            // A bad pointee must not hide unrelated fields. Retry bounded individual
            // spans only after a failed coalesced read involving dereferences.
            Err(_)
                if nodes
                    .iter()
                    .any(|n| n.steps.contains(&studio_dwarf::Step::Deref)) =>
            {
                return Ok(items
                    .into_iter()
                    .map(|item| {
                        let read = item.and_then(|item| {
                            let (address, len) = item.span().ok_or("not a number")?;
                            let bytes = self.read_once(vec![(address, len as usize)])?;
                            let bytes = bytes
                                .first()
                                .filter(|b| b.len() == len as usize)
                                .ok_or("short read")?;
                            Ok(ValueRead {
                                value: Some(decode(&item, bytes)).filter(|v| v.is_finite()),
                                text: exact_integer(&item, bytes),
                                error: None,
                            })
                        });
                        read.unwrap_or_else(|error| ValueRead {
                            value: None,
                            text: None,
                            error: Some(error),
                        })
                    })
                    .collect());
            }
            Err(error) => return Err(error),
        };
        Ok(items
            .into_iter()
            .map(|item| match item {
                Ok(item) => match item.span().and_then(|(address, len)| {
                    regions.iter().zip(&bytes).find_map(|(region, bytes)| {
                        let offset = address.checked_sub(region.address)? as usize;
                        bytes.get(offset..offset.checked_add(len as usize)?)
                    })
                }) {
                    Some(b) => ValueRead {
                        value: Some(decode(&item, b)).filter(|v| v.is_finite()),
                        text: exact_integer(&item, b),
                        error: None,
                    },
                    _ => ValueRead {
                        value: None,
                        text: None,
                        error: Some("short read".into()),
                    },
                },
                Err(error) => ValueRead {
                    value: None,
                    text: None,
                    error: Some(error),
                },
            })
            .collect())
    }

    /// Read target memory once through the running session.
    fn read_once(&self, regions: Vec<(u64, usize)>) -> Result<Vec<Vec<u8>>, String> {
        if regions.is_empty() {
            return Ok(Vec::new());
        }
        let (reply, rx) = mpsc::sync_channel(1);
        if !self.session.send(SessionCommand::Read { regions, reply }) {
            return Err("connect a debug probe first".into());
        }
        rx.recv_timeout(REQUEST_TIMEOUT)
            .unwrap_or_else(|_| Err("the target did not answer in time".into()))
    }
}

fn wait_reply_for(rx: mpsc::Receiver<Result<(), String>>, timeout: Duration) -> Result<(), String> {
    rx.recv_timeout(timeout)
        .unwrap_or_else(|_| Err("the target did not take the write in time".into()))
}

/// How recordings and the stream describe a watch: what the UI sent, filled in
/// from the ELF where it sent nothing.
fn watch_meta(elf: Option<&ElfInfo>, w: &WatchRequest) -> WatchMeta {
    let from_elf = || {
        w.node
            .as_ref()
            .and_then(|node| tree::node(elf?, node).ok())
            .map(|n| (n.path, n.type_name))
    };
    let (path, type_name) = match (&w.path, &w.type_name) {
        (Some(path), Some(type_name)) => (path.clone(), type_name.clone()),
        (path, type_name) => {
            let found = from_elf();
            (
                path.clone()
                    .or_else(|| found.as_ref().map(|f| f.0.clone()))
                    .or_else(|| w.cell.map(|c| format!("cell {c}")))
                    .unwrap_or_default(),
                type_name
                    .clone()
                    .or_else(|| found.map(|f| f.1))
                    .unwrap_or_default(),
            )
        }
    };
    let name = w
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| short_name(&path).to_string());
    WatchMeta {
        id: w.id,
        name,
        path,
        type_name,
        unit: w.unit.clone().filter(|u| !u.trim().is_empty()),
    }
}

/// `GIMBAL.yaw.angle` for `gimbal::GIMBAL.yaw.angle`, as the legend shows it
fn short_name(path: &str) -> &str {
    path.rfind("::").map_or(path, |at| &path[at + 2..])
}

fn tunable_names(catalog: &Catalog) -> Vec<TunableName> {
    catalog
        .entries
        .iter()
        .map(|e| TunableName {
            id: e.id,
            name: e.name.clone(),
            unit: Some(e.unit.clone()).filter(|u| !u.is_empty()),
        })
        .collect()
}

/// The ELF's GNU build id as hex, when it has one
fn build_id(elf: &[u8]) -> Option<String> {
    use object::Object;
    let file = object::File::parse(elf).ok()?;
    let id = file.build_id().ok()??;
    Some(id.iter().map(|b| format!("{b:02x}")).collect())
}

fn resolve_watch(
    elf: &ElfInfo,
    tuning: Option<&(TableLayout, Catalog)>,
    watch: &WatchRequest,
) -> Result<ReadItem, String> {
    match (&watch.node, watch.cell) {
        (Some(node), _) => resolve(elf, node),
        (None, Some(cell)) => tuning
            .and_then(|(_, catalog)| catalog.entry(cell))
            .map(|entry| entry.applied_item(0))
            .ok_or_else(|| "this ELF's tuning table has no such value".into()),
        (None, None) => Err("nothing to watch".into()),
    }
}

fn resolve(elf: &ElfInfo, node: &NodeRef) -> Result<ReadItem, String> {
    let n = tree::node(elf, node).map_err(|e| e.to_string())?;
    read_item(n)
}

fn read_item(n: SymbolNode) -> Result<ReadItem, String> {
    if !n.readable {
        return Err(n.status.unwrap_or_else(|| "not readable".into()));
    }
    let scalar = n
        .scalar
        .filter(|s| scalar_len(*s).is_some())
        .ok_or_else(|| format!("{} is not a single number", n.type_name))?;
    Ok(ReadItem {
        id: 0,
        address: n.address,
        scalar,
        bit_offset: n.bit_offset,
        bit_size: n.bit_size,
    })
}

fn collect_leaves(
    elf: &ElfInfo,
    node: SymbolNode,
    out: &mut Vec<SymbolNode>,
) -> Result<(), String> {
    if out.len() >= MAX_LEAVES || !node.readable {
        return Ok(());
    }
    match node.kind {
        NodeKind::Scalar | NodeKind::Enum => {
            if node.scalar.is_some_and(|s| scalar_len(s).is_some()) {
                out.push(node);
            }
        }
        // A tagged enum's payload depends on the live tag; only the tag is safe to sample
        NodeKind::TaggedEnum => {
            let children = tree::children(elf, &node.node, None).map_err(|e| e.to_string())?;
            if let Some(tag) = children
                .nodes
                .into_iter()
                .find(|c| c.node.steps.last() == Some(&studio_dwarf::Step::Discriminant))
            {
                collect_leaves(elf, tag, out)?;
            }
        }
        NodeKind::Struct | NodeKind::Array => {
            let children = tree::children(elf, &node.node, None).map_err(|e| e.to_string())?;
            for child in children.nodes {
                collect_leaves(elf, child, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Preserve integers that cannot round-trip through JSON's f64 number model.
fn exact_integer(item: &ReadItem, bytes: &[u8]) -> Option<String> {
    use studio_dwarf::VariableType;
    if item.bit_size.is_some() || item.bit_offset.is_some() {
        return None;
    }
    match item.scalar {
        VariableType::U64 => Some(u64::from_le_bytes(bytes.try_into().ok()?).to_string()),
        VariableType::I64 => Some(i64::from_le_bytes(bytes.try_into().ok()?).to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod inspector_tests {
    use super::*;
    use studio_dwarf::VariableType;

    #[test]
    fn integer_text_preserves_all_64_bits() {
        let mut item = ReadItem {
            id: 0,
            address: 0,
            scalar: VariableType::U64,
            bit_offset: None,
            bit_size: None,
        };
        assert_eq!(
            exact_integer(&item, &u64::MAX.to_le_bytes()).as_deref(),
            Some("18446744073709551615")
        );
        item.scalar = VariableType::I64;
        assert_eq!(
            exact_integer(&item, &i64::MIN.to_le_bytes()).as_deref(),
            Some("-9223372036854775808")
        );
        assert_eq!(exact_integer(&item, &[]), None);
    }
}
