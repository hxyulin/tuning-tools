#![cfg(unix)]
use std::{path::PathBuf, process::Command};
use studio_app::{
    can::{Request, Snapshot},
    StudioApp,
};
use studio_carriers::can::{ChannelConfig, Timing, Transmit};
fn library(dir: &std::path::Path) -> PathBuf {
    let out = dir.join(if cfg!(target_os = "macos") {
        "fake.dylib"
    } else {
        "fake.so"
    });
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dmcan.cpp");
    let result = Command::new("c++")
        .args(["-std=c++11", "-shared", "-fPIC"])
        .arg(source)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    out
}
fn config() -> ChannelConfig {
    ChannelConfig {
        channel: 0,
        fd: true,
        arbitration_bitrate: 1000000,
        data_bitrate: 2000000,
        arbitration_sample_point: 0.75,
        data_sample_point: 0.75,
        arbitration_timing: None,
        data_timing: None,
    }
}
fn send(app: &StudioApp) -> Snapshot {
    app.can_request(Request::Send {
        frame: Transmit {
            channel: 0,
            id: 0x123456,
            extended: true,
            fd: true,
            brs: true,
            rtr: false,
            data: (0..64).collect(),
        },
    })
    .unwrap();
    app.can_request(Request::Poll { after: 0 }).unwrap()
}
#[test]
fn native_abi_capture_recording_and_configuration_failures() {
    let dir = tempfile::tempdir().unwrap();
    let app = StudioApp::new()
        .with_can_worker_executable(PathBuf::from(env!("CARGO_BIN_EXE_studio-can-worker")));
    let s = app
        .can_request(Request::Scan {
            library: Some(library(dir.path())),
        })
        .unwrap();
    assert_eq!(s.devices.len(), 1);
    let mut bad = config();
    bad.arbitration_bitrate = 123;
    assert!(app
        .can_request(Request::Connect {
            index: 0,
            channels: 1,
            configs: vec![bad]
        })
        .is_err());
    assert!(
        !app.can_request(Request::Poll { after: 0 })
            .unwrap()
            .connected
    );
    app.can_request(Request::Connect {
        index: 0,
        channels: 1,
        configs: vec![config()],
    })
    .unwrap();
    assert!(app.can_request(Request::Scan { library: None }).is_err());
    let path = dir.path().join("capture.csv");
    app.can_request(Request::Record { path: path.clone() })
        .unwrap();
    let s = send(&app);
    assert_eq!(s.frames.len(), 2);
    assert_eq!(s.received, 1);
    assert_eq!(s.transmitted, 1);
    let f = &s.frames[1];
    assert_eq!(f.data, (0..64).collect::<Vec<_>>());
    assert_eq!(f.device_timestamp, u64::MAX.to_string());
    assert!(f.fd && f.extended && f.brs);
    assert_eq!(f.direction, "rx");
    assert_eq!(f.id, 0x123456);
    app.can_request(Request::StopRecording).unwrap();
    let csv = std::fs::read_to_string(path).unwrap();
    assert_eq!(csv.lines().count(), 3);
    assert!(csv.contains("18446744073709551615"));
    let path = dir.path().join("capture.mcap");
    app.can_request(Request::Record { path: path.clone() })
        .unwrap();
    send(&app);
    let end = app.can_request(Request::Disconnect).unwrap();
    assert!(!end.connected);
    assert!(end.recording.is_none());
    assert_eq!(end.recorded, 2);
    let bytes = std::fs::read(path).unwrap();
    let messages = mcap::MessageStream::new(&bytes)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].channel.topic, "can/frames");
    let value: serde_json::Value = serde_json::from_slice(&messages[1].data).unwrap();
    assert_eq!(value["data"].as_array().unwrap().len(), 64);
    let mut advanced = config();
    advanced.arbitration_timing = Some(Timing {
        seg1: 13,
        seg2: 2,
        sjw: 1,
        prescaler: 4,
    });
    advanced.data_timing = advanced.arbitration_timing.clone();
    app.can_request(Request::Connect {
        index: 0,
        channels: 1,
        configs: vec![advanced],
    })
    .unwrap();
    assert!(send(&app).connected);
    app.can_request(Request::Disconnect).unwrap();
    assert!(app
        .can_request(Request::Send {
            frame: Transmit {
                channel: 0,
                id: 1,
                extended: false,
                fd: false,
                brs: false,
                rtr: false,
                data: vec![]
            }
        })
        .is_err());
}

#[test]
fn sdk_process_failure_is_contained_and_scan_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let library = library(dir.path());
    let app = StudioApp::new()
        .with_can_worker_executable(PathBuf::from(env!("CARGO_BIN_EXE_studio-can-worker")));
    app.can_request(Request::Scan {
        library: Some(library.clone()),
    })
    .unwrap();
    app.can_request(Request::Connect {
        index: 0,
        channels: 1,
        configs: vec![config()],
    })
    .unwrap();
    assert!(app
        .can_request(Request::Send {
            frame: Transmit {
                channel: 0,
                id: 0x6ff,
                extended: false,
                fd: false,
                brs: false,
                rtr: false,
                data: vec![1],
            }
        })
        .is_err());
    assert!(
        !app.can_request(Request::Poll { after: 0 })
            .unwrap()
            .connected
    );
    app.can_request(Request::Scan {
        library: Some(library),
    })
    .unwrap();
    app.can_request(Request::Connect {
        index: 0,
        channels: 1,
        configs: vec![config()],
    })
    .unwrap();
    assert!(send(&app).connected);
    app.can_request(Request::Disconnect).unwrap();
}
