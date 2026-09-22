//! Descriptors for the values a host tool may watch and tune.
//!
//! A firmware declares one static per value and lists them in one [`Table`].
//! The host finds the table through the ELF's debug info, reads the descriptors
//! out of memory, samples the cells over SWD, and writes tunable requests into
//! them. The task that owns a value decides what it actually runs with:
//! [`Tunable::apply`] clamps a request to the declared range and moves toward
//! it by at most `max_step` per call, so a host write cannot push an unsafe
//! value even through a raw debug probe.
//!
//! # Layout contract
//!
//! The host decodes [`Table`] and [`Entry`] by field name from DWARF, not by a
//! fixed layout. Renaming or retyping a field, or changing what one means, is
//! a format change and bumps [`TABLE_VERSION`].
#![no_std]
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicU32, Ordering};

pub mod server;
pub mod store;
pub mod wire;

/// `Table::magic`: `RMTT` in little-endian byte order.
pub const TABLE_MAGIC: u32 = u32::from_le_bytes(*b"RMTT");
pub const TABLE_VERSION: u32 = 1;

/// How a cell's 32 bits are interpreted.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    F32 = 0,
    I32 = 1,
    U32 = 2,
    /// 0 or 1
    Bool = 3,
}

/// Who may change a value.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Access {
    /// Published by the firmware; host writes are ignored.
    ReadOnly = 0,
    /// The owner applies host requests while running, within range and step.
    Live = 1,
    /// As `Live`, but a framed write is refused unless the robot is safe.
    SafeOnly = 2,
}

/// Longest `Entry` name and unit, so one descriptor fits a catalog frame.
pub const MAX_NAME_LEN: usize = 96;
pub const MAX_UNIT_LEN: usize = 16;

/// One watchable or tunable value. Build it through [`Tunable`] or a watch
/// type, which fix the kind and access and give the firmware the matching
/// methods.
#[derive(Debug)]
pub struct Entry {
    /// FNV-1a of `name`: stable across builds while the name is.
    id: u32,
    kind: Kind,
    access: Access,
    /// Dotted path, for example `gimbal.pitch.encoder.angle.kp`.
    name: &'static str,
    /// Display unit, empty when dimensionless.
    unit: &'static str,
    /// Bits of the value the firmware was built with.
    default: u32,
    /// Inclusive range a request is clamped to. Infinite for watches.
    min: f32,
    max: f32,
    /// Largest change one [`Tunable::apply`] makes. Infinite for no limit.
    max_step: f32,
    /// Written by the host. The owner writes back only to correct a request it
    /// cannot take, so a host reading this sees what will be applied.
    requested: AtomicU32,
    /// Written by the owner: the value it is running with.
    applied: AtomicU32,
}

impl Entry {
    const fn new(
        name: &'static str,
        unit: &'static str,
        kind: Kind,
        access: Access,
        default: u32,
        range: (f32, f32, f32),
    ) -> Self {
        assert!(name.len() <= MAX_NAME_LEN && unit.len() <= MAX_UNIT_LEN);
        Self {
            id: fnv1a(name.as_bytes()),
            kind,
            access,
            name,
            unit,
            default,
            min: range.0,
            max: range.1,
            max_step: range.2,
            requested: AtomicU32::new(default),
            applied: AtomicU32::new(default),
        }
    }

    #[must_use]
    pub const fn id(&self) -> u32 {
        self.id
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    #[must_use]
    pub const fn unit(&self) -> &'static str {
        self.unit
    }

    #[must_use]
    pub const fn kind(&self) -> Kind {
        self.kind
    }

    #[must_use]
    pub const fn access(&self) -> Access {
        self.access
    }

    /// The value the owner last applied or published, as raw bits.
    #[must_use]
    pub fn applied_bits(&self) -> u32 {
        self.applied.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn requested_bits(&self) -> u32 {
        self.requested.load(Ordering::Relaxed)
    }

    #[must_use]
    pub const fn default_bits(&self) -> u32 {
        self.default
    }

    /// `(min, max, max_step)`; infinite for watches.
    #[must_use]
    pub const fn range(&self) -> (f32, f32, f32) {
        (self.min, self.max, self.max_step)
    }

    /// Whether a host may request a value for this entry.
    #[must_use]
    pub const fn is_tunable(&self) -> bool {
        !matches!(self.access, Access::ReadOnly)
    }

    fn store_request(&self, bits: u32) {
        self.requested.store(bits, Ordering::Relaxed);
    }
}

const WATCH_RANGE: (f32, f32, f32) = (f32::NEG_INFINITY, f32::INFINITY, f32::INFINITY);

/// An `f32` the host may change while the firmware runs.
#[derive(Debug)]
pub struct Tunable(Entry);

impl Tunable {
    /// `max_step` bounds the change per [`Tunable::apply`]: with one call per
    /// control tick, `(max - min) / max_step` ticks cross the whole range.
    ///
    /// # Panics
    ///
    /// At compile time, when used in a static, if the range is not finite and
    /// ordered, `default` lies outside it, or `max_step` is not positive.
    #[must_use]
    pub const fn new(
        name: &'static str,
        unit: &'static str,
        default: f32,
        min: f32,
        max: f32,
        max_step: f32,
    ) -> Self {
        assert!(min.is_finite() && max.is_finite() && min <= max);
        assert!(default >= min && default <= max);
        assert!(max_step > 0.0);
        Self(Entry::new(
            name,
            unit,
            Kind::F32,
            Access::Live,
            default.to_bits(),
            (min, max, max_step),
        ))
    }

    /// Refuse framed writes unless the robot is safe (see [`server::Server`]).
    #[must_use]
    pub const fn safe_only(mut self) -> Self {
        self.0.access = Access::SafeOnly;
        self
    }

    #[must_use]
    pub const fn entry(&self) -> &Entry {
        &self.0
    }

    #[must_use]
    pub const fn default_value(&self) -> f32 {
        f32::from_bits(self.0.default)
    }

    /// The value to run with this tick, given the one running now.
    ///
    /// A request outside the range is clamped, and one that is not a number
    /// is replaced by `current`; either correction is written back so the
    /// host sees it. The result moves from `current` toward the request by at
    /// most `max_step`, and is recorded as applied.
    pub fn apply(&self, current: f32) -> f32 {
        let e = &self.0;
        let raw = e.requested.load(Ordering::Relaxed);
        let wanted = f32::from_bits(raw);
        let target = if wanted.is_finite() {
            wanted.clamp(e.min, e.max)
        } else if current.is_finite() {
            current.clamp(e.min, e.max)
        } else {
            f32::from_bits(e.default)
        };
        if target.to_bits() != raw {
            // A newer host write wins over the correction of an older one.
            let _ = e.requested.compare_exchange(
                raw,
                target.to_bits(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
        let next = if current.is_finite() {
            current + (target - current).clamp(-e.max_step, e.max_step)
        } else {
            target
        };
        e.applied.store(next.to_bits(), Ordering::Relaxed);
        next
    }

    /// Ask for `value` from the firmware itself, as a host write would.
    pub fn request(&self, value: f32) {
        self.0.requested.store(value.to_bits(), Ordering::Relaxed);
    }

    #[must_use]
    pub fn requested(&self) -> f32 {
        f32::from_bits(self.0.requested.load(Ordering::Relaxed))
    }
}

macro_rules! watch {
    ($(#[$doc:meta])* $name:ident, $ty:ty, $kind:ident, $bits:expr) => {
        $(#[$doc])*
        #[derive(Debug)]
        pub struct $name(Entry);

        impl $name {
            #[must_use]
            pub const fn new(name: &'static str, unit: &'static str) -> Self {
                Self(Entry::new(name, unit, Kind::$kind, Access::ReadOnly, 0, WATCH_RANGE))
            }

            #[must_use]
            pub const fn entry(&self) -> &Entry {
                &self.0
            }

            pub fn publish(&self, value: $ty) {
                let bits: fn($ty) -> u32 = $bits;
                self.0.applied.store(bits(value), Ordering::Relaxed);
            }
        }
    };
}

watch!(
    /// An `f32` the firmware publishes for the host to sample.
    WatchF32, f32, F32, f32::to_bits
);
watch!(
    /// An `i32` the firmware publishes for the host to sample.
    WatchI32, i32, I32, |v| v.cast_unsigned()
);
watch!(
    /// A `u32` the firmware publishes for the host to sample.
    WatchU32, u32, U32, |v| v
);
watch!(
    /// A `bool` the firmware publishes for the host to sample.
    WatchBool, bool, Bool, u32::from
);

/// The firmware's list of values. One per binary; the host finds it by type.
#[derive(Debug)]
pub struct Table {
    magic: u32,
    version: u32,
    entries: &'static [&'static Entry],
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TableError {
    /// Two names hash to one id, or one name is listed twice.
    DuplicateId {
        first: &'static str,
        second: &'static str,
    },
}

impl Table {
    #[must_use]
    pub const fn new(entries: &'static [&'static Entry]) -> Self {
        Self {
            magic: TABLE_MAGIC,
            version: TABLE_VERSION,
            entries,
        }
    }

    /// [`TABLE_MAGIC`] and [`TABLE_VERSION`] as built.
    #[must_use]
    pub const fn format(&self) -> (u32, u32) {
        (self.magic, self.version)
    }

    #[must_use]
    pub const fn entries(&self) -> &'static [&'static Entry] {
        self.entries
    }

    /// FNV-1a over every descriptor (not the values), so a host can tell
    /// whether a cached catalog still describes this build.
    #[must_use]
    pub fn fingerprint(&self) -> u32 {
        let mut hash = FNV_OFFSET;
        for e in self.entries {
            hash = fnv1a_extend(hash, &e.id.to_le_bytes());
            hash = fnv1a_extend(hash, &[e.kind as u8, e.access as u8]);
            hash = fnv1a_extend(hash, &e.default.to_le_bytes());
            hash = fnv1a_extend(hash, &e.min.to_bits().to_le_bytes());
            hash = fnv1a_extend(hash, &e.max.to_bits().to_le_bytes());
            hash = fnv1a_extend(hash, &e.max_step.to_bits().to_le_bytes());
            hash = fnv1a_extend(hash, e.unit.as_bytes());
        }
        hash
    }

    /// Check what a `const` cannot: ids are unique.
    ///
    /// Call it at boot as well as in a test. The call is also what keeps the
    /// table in the image: nothing else references it, and the linker drops
    /// unreferenced statics.
    pub fn validate(&self) -> Result<(), TableError> {
        let entries = core::hint::black_box(self).entries;
        for (i, a) in entries.iter().enumerate() {
            if let Some(b) = entries[..i].iter().find(|b| b.id == a.id) {
                return Err(TableError::DuplicateId {
                    first: b.name,
                    second: a.name,
                });
            }
        }
        Ok(())
    }
}

const FNV_OFFSET: u32 = 0x811c_9dc5;

/// 32-bit FNV-1a.
#[must_use]
pub const fn fnv1a(bytes: &[u8]) -> u32 {
    fnv1a_extend(FNV_OFFSET, bytes)
}

const fn fnv1a_extend(mut hash: u32, bytes: &[u8]) -> u32 {
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u32;
        hash = hash.wrapping_mul(0x0100_0193);
        i += 1;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    static GAIN: Tunable = Tunable::new("test.gain", "", 2.0, 0.0, 10.0, 0.5);
    static ANGLE: WatchF32 = WatchF32::new("test.angle_rad", "rad");
    static TICKS: WatchU32 = WatchU32::new("test.ticks", "");
    static TABLE: Table = Table::new(&[GAIN.entry(), ANGLE.entry(), TICKS.entry()]);

    fn tunable(default: f32) -> Tunable {
        Tunable::new("t", "", default, -10.0, 10.0, 1.0)
    }

    #[test]
    fn fnv1a_matches_the_reference_vectors() {
        assert_eq!(fnv1a(b""), 0x811c_9dc5);
        assert_eq!(fnv1a(b"a"), 0xe40c_292c);
        assert_eq!(fnv1a(b"foobar"), 0xbf9c_f968);
    }

    #[test]
    fn a_table_of_distinct_names_validates() {
        assert_eq!(TABLE.validate(), Ok(()));
        assert_eq!(TABLE.entries().len(), 3);
        assert_eq!(TABLE.format(), (u32::from_le_bytes(*b"RMTT"), 1));
        assert_eq!(GAIN.entry().id(), fnv1a(b"test.gain"));
        assert_eq!(GAIN.entry().access(), Access::Live);
        assert_eq!(ANGLE.entry().kind(), Kind::F32);
        assert_eq!(TICKS.entry().kind(), Kind::U32);
    }

    #[test]
    fn a_repeated_name_is_refused() {
        static A: Tunable = Tunable::new("same", "", 0.0, 0.0, 1.0, 1.0);
        static B: WatchBool = WatchBool::new("same", "");
        static DUP: Table = Table::new(&[A.entry(), B.entry()]);
        assert_eq!(
            DUP.validate(),
            Err(TableError::DuplicateId {
                first: "same",
                second: "same"
            })
        );
    }

    #[test]
    fn with_no_request_the_default_is_applied() {
        let t = tunable(3.0);
        assert_eq!(t.apply(3.0), 3.0);
        assert_eq!(f32::from_bits(t.entry().applied_bits()), 3.0);
    }

    #[test]
    fn a_request_is_approached_by_at_most_one_step_per_apply() {
        let t = tunable(0.0);
        t.request(2.5);
        let mut value = 0.0;
        let mut seen = [0.0; 4];
        for slot in &mut seen {
            value = t.apply(value);
            *slot = value;
        }
        assert_eq!(seen, [1.0, 2.0, 2.5, 2.5]);
        assert_eq!(t.requested(), 2.5);
    }

    #[test]
    fn an_out_of_range_request_is_clamped_and_written_back() {
        let t = tunable(0.0);
        t.request(50.0);
        assert_eq!(t.apply(9.5), 10.0);
        assert_eq!(t.requested(), 10.0);
        t.request(-50.0);
        assert_eq!(t.apply(-9.5), -10.0);
        assert_eq!(t.requested(), -10.0);
    }

    #[test]
    fn a_request_that_is_not_a_number_keeps_the_current_value() {
        let t = tunable(1.0);
        t.request(f32::NAN);
        assert_eq!(t.apply(4.0), 4.0);
        assert_eq!(t.requested(), 4.0);
        t.request(f32::INFINITY);
        assert_eq!(t.apply(4.0), 4.0);
    }

    #[test]
    fn a_current_value_that_is_not_a_number_jumps_to_the_target() {
        let t = tunable(1.0);
        t.request(5.0);
        assert_eq!(t.apply(f32::NAN), 5.0);
        t.request(f32::NAN);
        assert_eq!(t.apply(f32::NAN), 1.0, "falls back to the default");
    }

    #[test]
    fn watches_publish_their_bits() {
        ANGLE.publish(-1.5);
        assert_eq!(f32::from_bits(ANGLE.entry().applied_bits()), -1.5);
        TICKS.publish(7);
        assert_eq!(TICKS.entry().applied_bits(), 7);
        let signed = WatchI32::new("s", "");
        signed.publish(-2);
        assert_eq!(signed.entry().applied_bits(), 0xffff_fffe);
        let flag = WatchBool::new("b", "");
        flag.publish(true);
        assert_eq!(flag.entry().applied_bits(), 1);
    }
}
