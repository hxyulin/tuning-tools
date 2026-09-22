//! Versioned SWD trace ABI from studio-task-trace; torn/overwritten slots are omitted.
use serde::Serialize;
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEvent {
    pub seq: u32,
    pub ticks: u64,
    pub task: u32,
    pub kind: u32,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceSnapshot {
    pub clock_hz: u32,
    pub head: u32,
    pub capacity: u32,
    pub incomplete: u32,
    pub events: Vec<TraceEvent>,
}
pub fn decode(bytes: &[u8]) -> Result<TraceSnapshot, String> {
    let word = |offset: usize| {
        bytes
            .get(offset..offset + 4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or("short trace read".to_string())
    };
    if word(0)? != 0x53545452 || word(4)? != 1 {
        return Err("unsupported task trace format".into());
    }
    let capacity = word(8)?;
    let clock_hz = word(12)?;
    let head = word(16)?;
    if capacity == 0 || capacity > 8192 || bytes.len() != 20 + capacity as usize * 24 {
        return Err("invalid trace capacity".into());
    }
    let mut events = Vec::new();
    let mut incomplete = 0;
    let next_slot = 20 + head as usize % capacity as usize * 24;
    let count = if head >= capacity || word(next_slot)? != 0 {
        capacity
    } else {
        head
    };
    for age in (0..count).rev() {
        let seq = head.wrapping_sub(age);
        let at = 20 + seq.wrapping_sub(1) as usize % capacity as usize * 24;
        if word(at)? != seq || word(at + 20)? != seq {
            incomplete += 1;
            continue;
        }
        let kind = word(at + 16)?;
        if !(1..=4).contains(&kind) {
            incomplete += 1;
            continue;
        }
        events.push(TraceEvent {
            seq,
            ticks: u64::from(word(at + 4)?) | u64::from(word(at + 8)?) << 32,
            task: word(at + 12)?,
            kind,
        });
    }
    Ok(TraceSnapshot {
        clock_hz,
        head,
        capacity,
        incomplete,
        events,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sequence_rollover_keeps_the_previous_ring_events() {
        let mut bytes = vec![0u8; 20 + 48];
        for (offset, value) in [
            (0, 0x53545452u32),
            (4, 1),
            (8, 2),
            (12, 1000),
            (16, 0),
            (20, u32::MAX),
            (24, 10),
            (32, 42),
            (36, 2),
            (40, u32::MAX),
            (44, 0),
            (48, 20),
            (56, 42),
            (60, 3),
            (64, 0),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        let snapshot = decode(&bytes).unwrap();
        assert_eq!(
            snapshot.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [u32::MAX, 0]
        );
        assert_eq!(snapshot.incomplete, 0);
    }

    #[test]
    fn rejects_torn_slots_and_checks_header() {
        let mut b = vec![0u8; 20 + 48];
        let put = |b: &mut Vec<u8>, i: usize, v: u32| b[i..i + 4].copy_from_slice(&v.to_le_bytes());
        for (i, v) in [
            (0, 0x53545452),
            (4, 1),
            (8, 2),
            (12, 64_000_000),
            (16, 2),
            (20, 1),
            (24, 100),
            (32, 42),
            (36, 2),
            (40, 1),
            (44, 2),
            (48, 200),
            (56, 42),
            (60, 3),
            (64, 99),
        ] {
            put(&mut b, i, v);
        }
        let r = decode(&b).unwrap();
        assert_eq!(r.events.len(), 1);
        assert_eq!(r.incomplete, 1);
        assert_eq!(r.events[0].ticks, 100);
        put(&mut b, 8, 9000);
        assert!(decode(&b).is_err());
    }
}
