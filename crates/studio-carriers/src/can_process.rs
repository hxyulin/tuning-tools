//! Process boundary around the proprietary SDK: a crash cannot take down Studio.
//! Vendor stdout is redirected away from the JSON pipe before loading the SDK.
use crate::can::{Adapter, ChannelConfig, Device, Frame, Transmit};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::Duration,
};

pub const WORKER_FLAG: &str = "--studio-can-sdk-worker";
#[derive(Serialize, Deserialize)]
enum Request {
    Scan {
        library: Option<PathBuf>,
    },
    Start {
        index: i32,
        channels: u8,
        configs: Vec<ChannelConfig>,
    },
    Send {
        frame: Transmit,
    },
    Poll,
    Stop,
}
#[derive(Default, Serialize, Deserialize)]
struct Response {
    devices: Vec<Device>,
    frames: Vec<Frame>,
    dropped: u64,
    error: Option<String>,
}

pub struct RemoteAdapter {
    child: Child,
    input: ChildStdin,
    responses: Receiver<Result<Response, String>>,
    reader: Option<JoinHandle<()>>,
    stopped: bool,
    pending: Vec<Frame>,
    dropped: u64,
    error: Option<String>,
}
impl RemoteAdapter {
    pub fn load(
        library: Option<&Path>,
        executable: Option<&Path>,
    ) -> Result<(Self, Vec<Device>), String> {
        let executable = executable
            .map(PathBuf::from)
            .map(Ok)
            .unwrap_or_else(std::env::current_exe)
            .map_err(|e| e.to_string())?;
        let mut command = Command::new(executable);
        command
            .arg(WORKER_FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("Cannot start isolated CAN SDK worker: {e}"))?;
        let input = child.stdin.take().ok_or("Missing SDK worker input")?;
        let mut output = BufReader::new(child.stdout.take().ok_or("Missing SDK worker output")?);
        let (tx, responses) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || loop {
            let mut line = String::new();
            let response = match output.read_line(&mut line) {
                Ok(0) => Err("Damiao SDK worker exited; disconnect and scan again".into()),
                Ok(_) => serde_json::from_str(&line)
                    .map_err(|e| format!("Invalid SDK worker response: {e}")),
                Err(e) => Err(e.to_string()),
            };
            let failed = response.is_err();
            if tx.send(response).is_err() || failed {
                break;
            }
        });
        let mut adapter = Self {
            child,
            input,
            responses,
            reader: Some(reader),
            stopped: false,
            pending: vec![],
            dropped: 0,
            error: None,
        };
        let response = adapter.call(Request::Scan {
            library: library.map(PathBuf::from),
        })?;
        Ok((adapter, response.devices))
    }
    fn call(&mut self, request: Request) -> Result<Response, String> {
        let result: Result<Response, String> = (|| {
            serde_json::to_writer(&mut self.input, &request).map_err(|e| e.to_string())?;
            self.input
                .write_all(b"\n")
                .and_then(|_| self.input.flush())
                .map_err(|e| e.to_string())?;
            let response = self
                .responses
                .recv_timeout(Duration::from_secs(10))
                .map_err(|_| {
                    "Damiao SDK worker did not respond within 10 seconds; scan again".to_string()
                })??;
            self.dropped = response.dropped;
            Ok(response)
        })();
        if let Err(e) = &result {
            self.error = Some(e.clone());
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.stopped = true;
        }
        let response = result?;
        if let Some(error) = &response.error {
            return Err(error.clone());
        }
        Ok(response)
    }
    pub fn start(
        &mut self,
        index: i32,
        channels: u8,
        configs: Vec<ChannelConfig>,
    ) -> Result<(), String> {
        self.error = None;
        self.call(Request::Start {
            index,
            channels,
            configs,
        })
        .map(|_| ())
    }
    pub fn send(&mut self, frame: &Transmit) -> Result<(), String> {
        // Validation errors do not end an otherwise healthy capture.
        frame.validate()?;
        self.call(Request::Send {
            frame: frame.clone(),
        })
        .map(|_| ())
    }
    pub fn drain(&mut self) -> Vec<Frame> {
        if !self.pending.is_empty() {
            return std::mem::take(&mut self.pending);
        }
        if self.error.is_some() || self.stopped {
            return vec![];
        }
        self.call(Request::Poll)
            .map(|r| r.frames)
            .unwrap_or_default()
    }
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn stop(&mut self) {
        if self.error.is_none() && !self.stopped {
            if let Ok(r) = self.call(Request::Stop) {
                self.pending = r.frames;
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stopped = true;
    }
}
impl Drop for RemoteAdapter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Drop the receiver before joining: an EOF notification may be queued.
        let (_, replacement) = mpsc::channel();
        drop(std::mem::replace(&mut self.responses, replacement));
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Called before Tauri/probe initialization in a dedicated child process.
/// Exit without SDK destructors: SDK 1.1 dereferences invalid USB handles there.
/// The OS reclaims all handles, threads and memory when the child exits.
pub fn run_worker_stdio() -> ! {
    let result = worker();
    if let Err(error) = &result {
        eprintln!("CAN SDK worker: {error}");
    }
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}
fn protocol_output() -> Result<std::fs::File, String> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        let output = std::io::stdout()
            .as_fd()
            .try_clone_to_owned()
            .map_err(|e| e.to_string())?;
        // SAFETY: duplicate stderr onto stdout only in the dedicated worker,
        // before loading native code or creating other threads.
        if unsafe { libc::dup2(2, 1) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(output.into())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsHandle, AsRawHandle};
        let output = std::io::stdout()
            .as_handle()
            .try_clone_to_owned()
            .map_err(|e| e.to_string())?;
        #[link(name = "kernel32")]
        extern "system" {
            fn SetStdHandle(which: u32, handle: *mut std::ffi::c_void) -> i32;
        }
        // SAFETY: worker-only redirection before the vendor DLL is loaded; both
        // handles come from live standard streams. The protocol owns a duplicate.
        unsafe {
            libc::dup2(2, 1);
            if SetStdHandle(-11i32 as u32, std::io::stderr().as_raw_handle()) == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
        }
        Ok(output.into())
    }
}
fn worker() -> Result<(), String> {
    let mut output = protocol_output()?;
    // Deliberately suppress Drop in this process. See run_worker_stdio.
    let mut adapter: Option<std::mem::ManuallyDrop<Adapter>> = None;
    for line in std::io::stdin().lock().lines() {
        let request: Request =
            serde_json::from_str(&line.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let stop = matches!(request, Request::Stop);
        let mut response = Response::default();
        let result = (|| -> Result<(), String> {
            match request {
                Request::Scan { library } => {
                    if adapter.is_some() {
                        return Err("Scan requires a fresh SDK worker".into());
                    }
                    adapter = Some(std::mem::ManuallyDrop::new(Adapter::load(
                        library.as_deref(),
                    )?));
                    response.devices = adapter.as_mut().unwrap().discover()?;
                }
                Request::Start {
                    index,
                    channels,
                    configs,
                } => adapter
                    .as_mut()
                    .ok_or("Scan first")?
                    .start(index, channels, configs)?,
                Request::Send { frame } => adapter.as_ref().ok_or("Scan first")?.send(&frame)?,
                Request::Poll => {
                    if let Some(a) = &adapter {
                        response.frames = a.drain();
                    }
                }
                Request::Stop => {
                    if let Some(a) = &mut adapter {
                        a.stop();
                        loop {
                            let frames = a.drain();
                            if frames.is_empty() {
                                break;
                            }
                            response.frames.extend(frames);
                        }
                    }
                }
            }
            Ok(())
        })();
        response.dropped = adapter.as_ref().map_or(0, |a| a.dropped());
        response.error = result.err();
        serde_json::to_writer(&mut output, &response).map_err(|e| e.to_string())?;
        output
            .write_all(b"\n")
            .and_then(|_| output.flush())
            .map_err(|e| e.to_string())?;
        if stop {
            break;
        }
    }
    Ok(())
}
