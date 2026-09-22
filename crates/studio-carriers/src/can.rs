//! Damiao Device SDK 1.1 ABI. All SDK calls/handles stay on the owning worker.
//! The vendor's packed bitfields are decoded as bytes, never Rust references to
//! unaligned fields. Supported vendor binaries are little-endian x64/ARM64.
use libloading::Library;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    ffi::c_void,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex, OnceLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};
type Handle = *mut c_void;
type Callback = unsafe extern "C" fn(Handle, *const u8);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Timing {
    pub seg1: u8,
    pub seg2: u8,
    pub sjw: u8,
    pub prescaler: u8,
}
impl Timing {
    fn validate(&self) -> Result<(), String> {
        if self.seg1 == 0
            || self.seg2 == 0
            || self.sjw == 0
            || self.prescaler == 0
            || self.sjw > self.seg2
        {
            return Err("Timing values must be nonzero, with SJW ≤ segment 2".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelConfig {
    pub channel: u8,
    pub fd: bool,
    pub arbitration_bitrate: u32,
    pub data_bitrate: u32,
    pub arbitration_sample_point: f32,
    pub data_sample_point: f32,
    pub arbitration_timing: Option<Timing>,
    pub data_timing: Option<Timing>,
}
impl ChannelConfig {
    pub fn validate(&self, channels: u8) -> Result<(), String> {
        if self.channel >= channels {
            return Err("Channel is out of range for this adapter".into());
        }
        if self.arbitration_bitrate == 0 || (self.fd && self.data_bitrate == 0) {
            return Err("Bitrates must be greater than zero".into());
        }
        for sp in [self.arbitration_sample_point, self.data_sample_point] {
            if !sp.is_finite() || sp <= 0.0 || sp >= 1.0 {
                return Err("Sample points must be between 0 and 1 (exclusive)".into());
            }
        }
        if let Some(t) = &self.arbitration_timing {
            t.validate()?;
        }
        if let Some(t) = &self.data_timing {
            t.validate()?;
        }
        if self.arbitration_timing.is_some() && self.fd && self.data_timing.is_none() {
            return Err("Advanced CAN FD timing requires data timing too".into());
        }
        if self.arbitration_timing.is_none() && self.data_timing.is_some() {
            return Err("Data timing requires arbitration timing".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transmit {
    pub channel: u8,
    pub id: u32,
    pub extended: bool,
    pub fd: bool,
    pub brs: bool,
    pub rtr: bool,
    pub data: Vec<u8>,
}
impl Transmit {
    pub fn validate(&self) -> Result<(), String> {
        if self.id > if self.extended { 0x1fff_ffff } else { 0x7ff } {
            return Err("CAN identifier is out of range".into());
        }
        if self.data.len() > if self.fd { 64 } else { 8 } {
            return Err("Payload exceeds frame capacity".into());
        }
        if self.fd
            && ![0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64].contains(&self.data.len())
        {
            return Err(
                "CAN FD payload must use a valid DLC length (0–8, 12, 16, 20, 24, 32, 48, 64)"
                    .into(),
            );
        }
        if (self.brs && !self.fd) || (self.rtr && self.fd) || (self.rtr && !self.data.is_empty()) {
            return Err(
                "BRS requires FD; remote frames require classic CAN and an empty payload".into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    pub sequence: u64,
    /// Raw SDK timestamp; units/timebase are not specified by the vendor.
    pub device_timestamp: String,
    /// Host arrival time. Strings preserve nanoseconds in JavaScript.
    pub host_timestamp_ns: String,
    pub channel: u8,
    pub id: u32,
    pub extended: bool,
    pub fd: bool,
    pub brs: bool,
    pub rtr: bool,
    pub esi: bool,
    pub ack: bool,
    pub dlc: u8,
    pub direction: String,
    pub data: Vec<u8>,
}
fn decode(bytes: &[u8; 80], direction: &'static str) -> Frame {
    let id = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let flags = bytes[13];
    let dlc = flags >> 4;
    let len = [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64][dlc as usize];
    let rtr = id & (1 << 31) != 0;
    Frame {
        sequence: 0,
        device_timestamp: u64::from_le_bytes(bytes[4..12].try_into().unwrap()).to_string(),
        host_timestamp_ns: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_string(),
        channel: bytes[12],
        id: id & 0x1fff_ffff,
        extended: id & (1 << 30) != 0,
        esi: id & (1 << 29) != 0,
        fd: flags & 1 != 0,
        brs: flags & 4 != 0,
        ack: flags & 8 != 0,
        rtr,
        dlc,
        direction: direction.into(),
        data: bytes[16..16
            + if rtr {
                0
            } else if flags & 1 != 0 {
                len
            } else {
                len.min(8)
            }]
            .to_vec(),
    }
}
struct Queue {
    tx: SyncSender<Frame>,
    dropped: AtomicU64,
}
fn registry() -> &'static Mutex<HashMap<usize, Arc<Queue>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, Arc<Queue>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
unsafe fn receive(h: Handle, p: *const u8, direction: &'static str) {
    if p.is_null() {
        return;
    }
    let q = registry()
        .lock()
        .ok()
        .and_then(|r| r.get(&(h as usize)).cloned());
    if let Some(q) = q {
        // SDK owns a packed 80-byte frame for the duration of this callback.
        let frame = decode(
            &unsafe { std::ptr::read_unaligned(p.cast::<[u8; 80]>()) },
            direction,
        );
        if q.tx.try_send(frame).is_err() {
            q.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
unsafe extern "C" fn rx(h: Handle, p: *const u8) {
    unsafe { receive(h, p, "rx") }
}
unsafe extern "C" fn tx(h: Handle, p: *const u8) {
    unsafe { receive(h, p, "tx") }
}
unsafe extern "C" fn err(h: Handle, p: *const u8) {
    unsafe { receive(h, p, "error") }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Baud {
    channel: u8,
    fd: bool,
    arbitration: u32,
    data: u32,
    arbitration_sp: f32,
    data_sp: f32,
}
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
struct Details {
    channel: u8,
    fd: u8,
    seg1: u8,
    seg2: u8,
    sjw: u8,
    prescaler: u8,
    data_seg1: u8,
    data_seg2: u8,
    data_sjw: u8,
    data_prescaler: u8,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub index: i32,
    pub model: String,
    pub channels: u8,
    pub version: String,
}

pub(crate) struct Adapter {
    lib: Library,
    ctx: Handle,
    devices: Vec<(Handle, Device)>,
    active: Option<Handle>,
    enabled: Vec<ChannelConfig>,
    queue: Option<Arc<Queue>>,
    receiver: Option<Receiver<Frame>>,
}
impl Adapter {
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let name = if cfg!(target_os = "macos") {
            "libdm_device.dylib"
        } else if cfg!(windows) {
            "dm_device.dll"
        } else {
            "libdm_device.so"
        };
        let path = path
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("TUNING_STUDIO_DMCAN_LIBRARY").map(PathBuf::from))
            .unwrap_or_else(|| {
                let exe = std::env::current_exe().unwrap_or_default();
                let beside = exe.parent().unwrap_or(Path::new(".")).join(name);
                let resource = exe
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join("../Resources")
                    .join(name);
                if beside.exists() {
                    beside
                } else if resource.exists() {
                    resource
                } else {
                    PathBuf::from(name)
                }
            });
        let lib = unsafe { Library::new(&path) }.map_err(|e| format!("Cannot load Damiao SDK at {}: {e}. Select the SDK 1.1 library or set TUNING_STUDIO_DMCAN_LIBRARY.",path.display()))?;
        let adapter = Self {
            lib,
            ctx: std::ptr::null_mut(),
            devices: vec![],
            active: None,
            enabled: vec![],
            queue: None,
            receiver: None,
        };
        Ok(adapter)
    }
    unsafe fn sym<T: Copy>(&self, name: &[u8]) -> Result<T, String> {
        unsafe { self.lib.get::<T>(name) }
            .map(|s| *s)
            .map_err(|e| e.to_string())
    }
    pub fn discover(&mut self) -> Result<Vec<Device>, String> {
        if self.active.is_some() {
            return Err("Disconnect CAN before scanning".into());
        }
        if !self.devices.is_empty() {
            return Err("Use a fresh SDK worker to rescan".into());
        }
        unsafe {
            (self.sym::<unsafe extern "C" fn(*mut Handle)>(b"dmcan_context_create\0")?)(
                &mut self.ctx,
            );
        }
        if self.ctx.is_null() {
            return Err("Damiao SDK failed to create a context".into());
        }
        // Discover once: calling find-with-type repeatedly may invalidate earlier handles.
        let count = unsafe {
            (self.sym::<unsafe extern "C" fn(Handle) -> i32>(b"dmcan_find_devices\0")?)(self.ctx)
        };
        if count < 0 {
            return Err("Damiao device discovery failed".into());
        }
        for index in 0..count {
            let mut handle = std::ptr::null_mut();
            let ok = unsafe {
                (self.sym::<unsafe extern "C" fn(Handle, *mut Handle, i32) -> bool>(
                    b"dmcan_device_get\0",
                )?)(self.ctx, &mut handle, index)
            };
            if !ok || handle.is_null() {
                continue;
            }
            // Open USB immediately: delayed opening fails with this SDK. CAN
            // channels are configured/enabled only by start(). The helper owns
            // these handles until it exits; never destroy unopened SDK devices.
            if !unsafe {
                (self.sym::<unsafe extern "C" fn(Handle) -> bool>(b"dmcan_device_open\0")?)(handle)
            } {
                return Err(format!("Cannot open adapter #{index}; check USB permissions/driver and other CAN applications"));
            }
            // Public ABI has no structured device model/serial getter. Do not guess
            // the model from undocumented version text; user selects model at connect.
            let version = String::new();
            self.devices.push((
                handle,
                Device {
                    index,
                    model: "Damiao CAN adapter".into(),
                    channels: 0,
                    version,
                },
            ));
        }
        Ok(self.devices.iter().map(|(_, d)| d.clone()).collect())
    }
    pub fn start(
        &mut self,
        index: i32,
        channels: u8,
        configs: Vec<ChannelConfig>,
    ) -> Result<(), String> {
        if self.active.is_some() {
            return Err("Disconnect CAN before changing configuration".into());
        }
        if ![1, 2, 4].contains(&channels) || configs.is_empty() {
            return Err("Select an adapter model and at least one channel".into());
        }
        let mut seen = std::collections::HashSet::new();
        for c in &configs {
            c.validate(channels)?;
            if !seen.insert(c.channel) {
                return Err("Duplicate channel configuration".into());
            }
        }
        if !self.devices.iter().any(|(_, d)| d.index == index) {
            return Err("Scan and select a CAN adapter first".into());
        }
        let h = self
            .devices
            .iter()
            .find(|(_, d)| d.index == index)
            .map(|(h, _)| *h)
            .ok_or("Scan and select a CAN adapter first")?;
        self.active = Some(h);
        let (tx, rxq) = mpsc::sync_channel(65536);
        let q = Arc::new(Queue {
            tx,
            dropped: AtomicU64::new(0),
        });
        registry()
            .lock()
            .map_err(|_| "CAN registry poisoned")?
            .insert(h as usize, q.clone());
        self.queue = Some(q);
        self.receiver = Some(rxq);
        let result = (|| {
            unsafe {
                for c in configs {
                    // SDK 1.1 requires the channel to be enabled before timing
                    // commands. Track it first so every failure disables it.
                    self.enabled.push(c.clone());
                    if !(self.sym::<unsafe extern "C" fn(Handle, u8) -> bool>(
                        b"dmcan_device_enable_channel\0",
                    )?)(h, c.channel)
                    {
                        return Err(format!("Cannot enable channel {}", c.channel));
                    }
                    let baud = Baud {
                        channel: c.channel,
                        fd: c.fd,
                        arbitration: c.arbitration_bitrate,
                        data: c.data_bitrate,
                        arbitration_sp: c.arbitration_sample_point,
                        data_sp: c.data_sample_point,
                    };
                    let ok = if let Some(t) = &c.arbitration_timing {
                        let d = c.data_timing.as_ref().unwrap_or(t);
                        let details = Details {
                            channel: c.channel,
                            fd: c.fd as u8,
                            seg1: t.seg1,
                            seg2: t.seg2,
                            sjw: t.sjw,
                            prescaler: t.prescaler,
                            data_seg1: d.seg1,
                            data_seg2: d.seg2,
                            data_sjw: d.sjw,
                            data_prescaler: d.prescaler,
                        };
                        (self.sym::<unsafe extern "C" fn(Handle, u8, Details) -> bool>(
                            b"dmcan_device_set_channel_baudrate_details\0",
                        )?)(h, c.channel, details)
                    } else {
                        (self.sym::<unsafe extern "C" fn(Handle, u8, Baud) -> bool>(
                            b"dmcan_device_set_channel_baudrate\0",
                        )?)(h, c.channel, baud)
                    };
                    if !ok {
                        return Err(format!("Adapter rejected timing for channel {}", c.channel));
                    }
                }
                // Register frame callbacks only after configuration: the vendor
                // receive hook otherwise interferes with command replies.
                for (symbol, callback) in [
                    (
                        b"dmcan_device_hook_recv_callback\0".as_slice(),
                        rx as Callback,
                    ),
                    (
                        b"dmcan_device_hook_sent_callback\0".as_slice(),
                        tx_callback(),
                    ),
                    (
                        b"dmcan_device_hook_err_callback\0".as_slice(),
                        err as Callback,
                    ),
                ] {
                    (self.sym::<unsafe extern "C" fn(Handle, Callback)>(symbol)?)(h, callback);
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.stop();
        }
        result
    }
    pub fn send(&self, frame: &Transmit) -> Result<(), String> {
        frame.validate()?;
        let h = self.active.ok_or("CAN is disconnected")?;
        let config = self
            .enabled
            .iter()
            .find(|c| c.channel == frame.channel)
            .ok_or("Channel is not enabled")?;
        if frame.fd && !config.fd {
            return Err("CAN FD is not enabled on this channel".into());
        }
        let mut data = frame.data.clone();
        let ok = unsafe {
            (self.sym::<unsafe extern "C" fn(
                Handle,
                u8,
                u32,
                bool,
                bool,
                bool,
                bool,
                u8,
                *mut u8,
            ) -> bool>(b"dmcan_device_send_can\0")?)(
                h,
                frame.channel,
                frame.id,
                frame.fd,
                frame.extended,
                frame.rtr,
                frame.brs,
                data.len() as u8,
                data.as_mut_ptr(),
            )
        };
        if ok {
            Ok(())
        } else {
            Err("Adapter rejected CAN transmission".into())
        }
    }
    pub fn drain(&self) -> Vec<Frame> {
        self.receiver
            .as_ref()
            .map(|r| r.try_iter().take(4096).collect())
            .unwrap_or_default()
    }
    pub fn dropped(&self) -> u64 {
        self.queue
            .as_ref()
            .map_or(0, |q| q.dropped.load(Ordering::Relaxed))
    }
    pub fn stop(&mut self) {
        if let Some(h) = self.active.take() {
            unsafe {
                if let Ok(disable) = self.sym::<unsafe extern "C" fn(Handle, u8) -> bool>(
                    b"dmcan_device_disable_channel\0",
                ) {
                    for c in &self.enabled {
                        disable(h, c.channel);
                    }
                }
            }
            if let Ok(mut r) = registry().lock() {
                r.remove(&(h as usize));
            }
        }
        self.enabled.clear();
    }
}
fn tx_callback() -> Callback {
    tx
}
impl Drop for Adapter {
    fn drop(&mut self) {
        self.stop();
        if !self.ctx.is_null() {
            unsafe {
                if let Ok(destroy) =
                    self.sym::<unsafe extern "C" fn(Handle)>(b"dmcan_context_destroy\0")
                {
                    destroy(self.ctx);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packed_fd_frame_preserves_flags_timestamp_and_payload() {
        assert_eq!(std::mem::size_of::<Baud>(), 18);
        assert_eq!(std::mem::size_of::<Details>(), 10);
        let mut b = [0; 80];
        b[..4].copy_from_slice(&(0x123456u32 | (1 << 30) | (1 << 29)).to_le_bytes());
        b[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
        b[12] = 3;
        b[13] = 0xfd;
        for i in 0..64 {
            b[16 + i] = i as u8;
        }
        let f = decode(&b, "rx");
        assert_eq!(f.data.len(), 64);
        assert_eq!(f.id, 0x123456);
        assert_eq!(f.device_timestamp, u64::MAX.to_string());
        assert!(f.fd && f.brs && f.ack && f.extended && f.esi);
        assert_eq!(f.channel, 3);
    }
    #[test]
    fn rejects_invalid_transmissions() {
        let mut f = Transmit {
            channel: 0,
            id: 0x800,
            extended: false,
            fd: false,
            brs: false,
            rtr: false,
            data: vec![0; 8],
        };
        assert!(f.validate().is_err());
        f.extended = true;
        assert!(f.validate().is_ok());
        f.fd = true;
        f.data.push(0);
        assert!(f.validate().is_err());
        f.data.resize(12, 0);
        assert!(f.validate().is_ok());
        f.rtr = true;
        assert!(f.validate().is_err());
    }
}
