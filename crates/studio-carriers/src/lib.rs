//! Carriers: how the studio reaches a target.
//!
//! A probe carrier gives random access to target memory ([`MemoryAccess`]). The
//! log stream (RTT) and the core run state are read through that same memory,
//! so a probe needs one open memory interface and nothing else. A serial
//! carrier ([`ByteStream`]) moves the firmware's framed protocol instead. Every carrier is
//! owned by one hardware thread; the traits take `&mut self` and nothing here
//! is shared.

pub mod can;
pub mod can_process;
pub mod cortex_m;
pub mod mock;
pub mod probe;
pub mod rtt;
pub mod serial;

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum CarrierError {
    #[error("no debug probe matches `{0}`")]
    ProbeNotFound(String),
    #[error("could not open the probe: {0}")]
    Probe(String),
    #[error("could not attach to the target: {0}")]
    Attach(String),
    #[error("read of {len} bytes at {address:#010x} failed: {reason}")]
    Read {
        address: u64,
        len: usize,
        reason: String,
    },
    #[error("write of {len} bytes at {address:#010x} failed: {reason}")]
    Write {
        address: u64,
        len: usize,
        reason: String,
    },
    #[error("could not open {port}: {reason}")]
    Port { port: String, reason: String },
    #[error("the link to the target was lost: {0}")]
    Stream(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CarrierError>;

/// Random access to target memory. Reads must not halt the core.
pub trait MemoryAccess {
    fn read(&mut self, address: u64, buf: &mut [u8]) -> Result<()>;
    fn write(&mut self, address: u64, data: &[u8]) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum StreamState {
    /// The firmware has no stream (no RTT control block in the ELF)
    Absent,
    /// Looking for the control block; firmware may not have initialised it yet
    Searching,
    Attached {
        channel: String,
        /// The firmware blocks when the buffer is full, so a slow host stalls it
        blocking: bool,
    },
}

/// An ordered byte link to the firmware: USB CDC, a UART, or a test double.
pub trait ByteStream: Send {
    /// Read what has arrived, waiting briefly; `Ok(0)` when nothing did.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;
    fn write_all(&mut self, bytes: &[u8]) -> Result<()>;
}

/// Run state of the core, polled at a low rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CoreState {
    Running,
    Halted,
    Sleeping,
    LockedUp,
    Unknown,
}

/// A connected target.
pub trait Link: Send {
    /// Run `body` with target memory held open.
    ///
    /// Opening memory can be expensive (a probe re-reads the access port's
    /// registers, several USB round trips), so callers stay inside `body` for as
    /// long as they can and only come back out to reopen after persistent errors.
    fn with_memory(&mut self, body: &mut dyn FnMut(&mut dyn MemoryAccess)) -> Result<()>;
}
