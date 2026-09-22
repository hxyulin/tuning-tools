//! Saved requests: a record format for two erase blocks written in turn.
//!
//! ```text
//! magic u32 | version u16 | count u16 | generation u32 | count × (id u32, kind u8, 0 0 0, bits u32) | crc16
//! ```
//!
//! A save writes the slot that does not hold the newest valid record, with the
//! next generation, so power lost mid-write leaves the older record intact.
//! Values are matched by id and kind on restore, so a record survives a
//! rebuild that adds, removes or reorders values. The crate owns no flash
//! driver; the caller moves the bytes.

use crate::wire::{crc16, Overflow};
use crate::{Kind, Table};

pub const RECORD_MAGIC: u32 = u32::from_le_bytes(*b"RMTS");
pub const RECORD_VERSION: u16 = 1;
const HEADER_LEN: usize = 12;
const ITEM_LEN: usize = 12;
const CRC_LEN: usize = 2;

/// Bytes a record for `table` needs.
#[must_use]
pub fn record_len(table: &Table) -> usize {
    let tunables = table.entries().iter().filter(|e| e.is_tunable()).count();
    HEADER_LEN + tunables * ITEM_LEN + CRC_LEN
}

/// Write every tunable's current request into `out`; returns the record length.
///
/// # Errors
///
/// If `out` is shorter than [`record_len`].
pub fn encode(table: &Table, generation: u32, out: &mut [u8]) -> Result<usize, Overflow> {
    let len = record_len(table);
    let count = u16::try_from((len - HEADER_LEN - CRC_LEN) / ITEM_LEN).map_err(|_| Overflow)?;
    let record = out.get_mut(..len).ok_or(Overflow)?;
    record[..4].copy_from_slice(&RECORD_MAGIC.to_le_bytes());
    record[4..6].copy_from_slice(&RECORD_VERSION.to_le_bytes());
    record[6..8].copy_from_slice(&count.to_le_bytes());
    record[8..12].copy_from_slice(&generation.to_le_bytes());
    let (items, _) = record[HEADER_LEN..len - CRC_LEN].as_chunks_mut::<ITEM_LEN>();
    for (item, e) in items
        .iter_mut()
        .zip(table.entries().iter().filter(|e| e.is_tunable()))
    {
        item[..4].copy_from_slice(&e.id().to_le_bytes());
        item[4..8].copy_from_slice(&[e.kind() as u8, 0, 0, 0]);
        item[8..].copy_from_slice(&e.requested_bits().to_le_bytes());
    }
    let crc = crc16(&record[..len - CRC_LEN]);
    record[len - CRC_LEN..].copy_from_slice(&crc.to_le_bytes());
    Ok(len)
}

/// A valid record found at the start of a slot.
#[derive(Copy, Clone, Debug)]
pub struct Record<'a> {
    pub generation: u32,
    items: &'a [u8],
}

impl<'a> Record<'a> {
    /// Parse the record at the start of `bytes`; anything after it is ignored.
    /// Erased flash, a torn write or another format reads as `None`.
    #[must_use]
    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        let header = bytes.get(..HEADER_LEN)?;
        let field = |at: usize| {
            u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
        };
        if field(0) != RECORD_MAGIC || u16::from_le_bytes([header[4], header[5]]) != RECORD_VERSION
        {
            return None;
        }
        let count = usize::from(u16::from_le_bytes([header[6], header[7]]));
        let body = HEADER_LEN + count * ITEM_LEN;
        let crc = bytes.get(body..body + CRC_LEN)?;
        if u16::from_le_bytes([crc[0], crc[1]]) != crc16(&bytes[..body]) {
            return None;
        }
        Some(Self {
            generation: field(8),
            items: &bytes[HEADER_LEN..body],
        })
    }

    /// Saved `(id, kind tag, bits)` in record order.
    pub fn items(&self) -> impl Iterator<Item = (u32, u8, u32)> + 'a {
        self.items.as_chunks::<ITEM_LEN>().0.iter().map(|item| {
            (
                u32::from_le_bytes([item[0], item[1], item[2], item[3]]),
                item[4],
                u32::from_le_bytes([item[8], item[9], item[10], item[11]]),
            )
        })
    }

    /// Request each saved value that still names a tunable of the same kind
    /// and, for `f32`, is finite and in range. Returns how many were restored.
    ///
    /// The owner still applies the request at its declared step, so a restored
    /// value is reached from the code default over the first ticks.
    pub fn restore(&self, table: &Table) -> usize {
        let mut restored = 0;
        for (id, kind, bits) in self.items() {
            let Some(e) = table
                .entries()
                .iter()
                .find(|e| e.id() == id && e.is_tunable())
            else {
                continue;
            };
            if e.kind() as u8 != kind {
                continue;
            }
            if e.kind() == Kind::F32 {
                let value = f32::from_bits(bits);
                let (min, max, _) = e.range();
                if !(value.is_finite() && value >= min && value <= max) {
                    continue;
                }
            }
            e.store_request(bits);
            restored += 1;
        }
        restored
    }
}

/// One of the two slots a store alternates between.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    A,
    B,
}

/// Where to load from and where the next save goes, given the generation of
/// the valid record in each slot.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The slot holding the newest record, if either is valid.
    pub newest: Option<Slot>,
    /// The slot to overwrite: never the newest.
    pub target: Slot,
    pub next_generation: u32,
}

#[must_use]
pub fn plan(a: Option<u32>, b: Option<u32>) -> Plan {
    let newest = match (a, b) {
        (None, None) => None,
        (Some(_), None) => Some(Slot::A),
        (None, Some(_)) => Some(Slot::B),
        (Some(a), Some(b)) => Some(if b > a { Slot::B } else { Slot::A }),
    };
    let next_generation = a.max(b).map_or(1, |g| g.wrapping_add(1));
    let target = if newest == Some(Slot::A) {
        Slot::B
    } else {
        Slot::A
    };
    Plan {
        newest,
        target,
        next_generation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Tunable, WatchF32};

    static GAIN: Tunable = Tunable::new("st.gain", "", 2.0, 0.0, 10.0, 0.5);
    static LIMIT: Tunable = Tunable::new("st.limit", "A", 1.0, 0.0, 5.0, 0.1);
    static ANGLE: WatchF32 = WatchF32::new("st.angle", "rad");
    static TABLE: Table = Table::new(&[GAIN.entry(), ANGLE.entry(), LIMIT.entry()]);
    // A rebuild: one value dropped, one added, same name kept.
    static R_GAIN: Tunable = Tunable::new("st.gain", "", 2.0, 0.0, 4.0, 0.5);
    static R_NEW: Tunable = Tunable::new("st.new", "", 7.0, 0.0, 9.0, 0.5);
    static REBUILT: Table = Table::new(&[R_NEW.entry(), R_GAIN.entry()]);

    #[test]
    fn a_record_round_trips_and_ignores_trailing_erased_bytes() {
        GAIN.request(3.5);
        LIMIT.request(0.25);
        let mut flash = [0xff; 64];
        let len = encode(&TABLE, 7, &mut flash).unwrap();
        assert_eq!(len, record_len(&TABLE));
        let record = Record::decode(&flash).unwrap();
        assert_eq!(record.generation, 7);
        let items: [(u32, u8, u32); 2] = {
            let mut it = record.items();
            [it.next().unwrap(), it.next().unwrap()]
        };
        assert_eq!(items[0], (GAIN.entry().id(), 0, 3.5f32.to_bits()));
        assert_eq!(items[1], (LIMIT.entry().id(), 0, 0.25f32.to_bits()));
    }

    #[test]
    fn erased_torn_and_short_records_are_rejected() {
        assert!(Record::decode(&[0xff; 64]).is_none());
        let mut flash = [0u8; 64];
        let len = encode(&TABLE, 1, &mut flash).unwrap();
        assert!(Record::decode(&flash[..len - 1]).is_none());
        flash[14] ^= 1;
        assert!(Record::decode(&flash).is_none());
        assert_eq!(encode(&TABLE, 1, &mut [0; 8]), Err(Overflow));
    }

    #[test]
    fn restore_matches_by_id_and_skips_out_of_range_values() {
        let mut out = [0u8; 64];
        // Saved by the old build: gain 6.0, which the rebuild's range no longer allows.
        out[..4].copy_from_slice(&RECORD_MAGIC.to_le_bytes());
        out[4..6].copy_from_slice(&RECORD_VERSION.to_le_bytes());
        out[6..8].copy_from_slice(&2u16.to_le_bytes());
        out[12..16].copy_from_slice(&R_GAIN.entry().id().to_le_bytes());
        out[20..24].copy_from_slice(&6.0f32.to_bits().to_le_bytes());
        out[24..28].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        out[32..36].copy_from_slice(&1.0f32.to_bits().to_le_bytes());
        let crc = crc16(&out[..36]);
        out[36..38].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(Record::decode(&out).unwrap().restore(&REBUILT), 0);
        assert_eq!(R_GAIN.requested(), 2.0);

        out[20..24].copy_from_slice(&3.0f32.to_bits().to_le_bytes());
        let crc = crc16(&out[..36]);
        out[36..38].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(Record::decode(&out).unwrap().restore(&REBUILT), 1);
        assert_eq!(R_GAIN.requested(), 3.0);
        assert_eq!(R_NEW.requested(), 7.0);
    }

    #[test]
    fn saves_alternate_slots_and_never_overwrite_the_newest() {
        assert_eq!(
            plan(None, None),
            Plan {
                newest: None,
                target: Slot::A,
                next_generation: 1
            }
        );
        assert_eq!(plan(Some(1), None).target, Slot::B);
        assert_eq!(plan(Some(1), Some(2)).newest, Some(Slot::B));
        assert_eq!(plan(Some(1), Some(2)).target, Slot::A);
        assert_eq!(plan(Some(1), Some(2)).next_generation, 3);
        assert_eq!(plan(None, Some(5)).target, Slot::A);
        assert_eq!(plan(Some(9), Some(4)).target, Slot::B);
    }
}
