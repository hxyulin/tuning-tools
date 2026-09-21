//! Inspector reads share the probe worker and coalesce independently of plot subscriptions.
use std::sync::{Arc, Mutex};
use studio_app::{Carrier, ConnectRequest, SessionEvent, SessionSink, StudioApp};
use studio_carriers::{mock::MockLink, Link, MemoryAccess, Result};
use studio_dwarf::NodeRef;

type Reads = Arc<Mutex<Vec<(u64, usize)>>>;
struct Counted {
    memory: MockLink,
    reads: Reads,
}
struct CountedMemory<'a> {
    memory: &'a mut dyn MemoryAccess,
    reads: Reads,
}
impl MemoryAccess for CountedMemory<'_> {
    fn read(&mut self, address: u64, bytes: &mut [u8]) -> Result<()> {
        self.reads.lock().unwrap().push((address, bytes.len()));
        self.memory.read(address, bytes)
    }
    fn write(&mut self, address: u64, bytes: &[u8]) -> Result<()> {
        self.memory.write(address, bytes)
    }
}
impl Link for Counted {
    fn with_memory(&mut self, body: &mut dyn FnMut(&mut dyn MemoryAccess)) -> Result<()> {
        let reads = self.reads.clone();
        self.memory.with_memory(&mut |memory| {
            body(&mut CountedMemory {
                memory,
                reads: reads.clone(),
            })
        })
    }
}
struct Sink;
impl SessionSink for Sink {
    fn frame(&self, _: Vec<u8>) -> bool {
        true
    }
    fn event(&self, _: SessionEvent) {}
}

#[test]
fn inspector_coalesces_preserves_order_and_does_not_need_plots() {
    let memory = MockLink::new();
    memory.poke(0x2000_0004, &42u32.to_le_bytes());
    memory.poke(0x2000_0008, &1.5f32.to_le_bytes());
    let reads: Reads = Arc::default();
    let log = reads.clone();
    let target = memory.clone();
    let app = StudioApp::with_probe_opener(Arc::new(move |_, _| {
        Ok(Box::new(Counted {
            memory: target.clone(),
            reads: log.clone(),
        }))
    }))
    .without_rtt();
    app.open_elf(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../studio-dwarf/tests/fixtures/test_arm.elf"
        )
        .into(),
    )
    .unwrap();
    app.connect(
        ConnectRequest {
            carrier: Carrier::Probe,
            chip: "mock".into(),
            probe: None,
            port: None,
            speed_khz: None,
            rate_hz: 100.0,
        },
        Arc::new(Sink),
    )
    .unwrap();
    let nodes = [
        NodeRef::root("sensor_data"),
        NodeRef::root("missing"),
        NodeRef::root("global_counter"),
        NodeRef::root("sensor_data"),
    ];
    let values = serde_json::to_value(app.read_values(&nodes).unwrap()).unwrap();
    assert_eq!(values[0]["value"], 1.5);
    assert!(values[1]["error"].is_string());
    assert_eq!(values[2]["value"], 42.0);
    assert_eq!(values[3]["value"], 1.5);
    let ram_reads: Vec<_> = reads
        .lock()
        .unwrap()
        .iter()
        .copied()
        .filter(|(a, _)| (0x2000_0000..0x2000_0010).contains(a))
        .collect();
    assert_eq!(
        ram_reads,
        vec![(0x2000_0004, 8)],
        "neighbours and duplicate requests share one memory read"
    );
    memory.poke(0x2000_0004, &99u32.to_le_bytes());
    assert_eq!(
        serde_json::to_value(app.read_values(&nodes).unwrap()).unwrap()[2]["value"],
        99.0
    );
    assert!(app
        .read_values(&vec![NodeRef::root("global_counter"); 257])
        .err()
        .unwrap()
        .contains("at most"));
    app.disconnect();
    assert!(app.read_values(&nodes).is_err());
}
