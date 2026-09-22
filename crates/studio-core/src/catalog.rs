//! The firmware's value catalog: the `rm_telemetry::Table` a build declares.
//!
//! The table is found by type in the ELF's DWARF and decoded by field name, so
//! the host follows the firmware's layout rather than hard-coding one. Every
//! descriptor is an initialised static, so the catalog decodes from the ELF
//! file alone ([`ElfImage`]); a probe is needed only for live values and writes.

use object::{Object, ObjectSection, SectionKind};
use serde::Serialize;
use studio_carriers::{CarrierError, MemoryAccess};
use studio_dwarf::{ElfInfo, TypeHandle, VariableType};

use crate::plan::ReadItem;

/// `RMTT`, little-endian
pub const TABLE_MAGIC: u32 = u32::from_le_bytes(*b"RMTT");
pub const TABLE_VERSION: u32 = tuning_studio_api::TABLE_VERSION;
/// Refuse tables larger than any firmware would declare; guards against garbage
const MAX_ENTRIES: u64 = 4096;
const MAX_TEXT: u64 = 256;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("the tuning table's type is not what this version reads: {0}")]
    Layout(String),
    #[error("the tuning table at {address:#010x} is not valid: {reason}")]
    Invalid { address: u64, reason: String },
    #[error(transparent)]
    Memory(#[from] CarrierError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CellKind {
    F32,
    I32,
    U32,
    Bool,
}

impl CellKind {
    /// `rm_telemetry::Kind` discriminant
    pub fn from_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => Self::F32,
            1 => Self::I32,
            2 => Self::U32,
            3 => Self::Bool,
            _ => return None,
        })
    }

    pub fn tag(self) -> u8 {
        match self {
            Self::F32 => 0,
            Self::I32 => 1,
            Self::U32 => 2,
            Self::Bool => 3,
        }
    }

    pub fn decode(self, bits: u32) -> f64 {
        match self {
            Self::F32 => widen(f32::from_bits(bits)),
            Self::I32 => f64::from(bits as i32),
            Self::U32 | Self::Bool => f64::from(bits),
        }
    }

    /// Bits for `value`, or why the cell cannot hold it.
    pub fn encode(self, value: f64) -> Result<u32, String> {
        if !value.is_finite() {
            return Err("the value is not a number".into());
        }
        match self {
            Self::F32 => Ok((value as f32).to_bits()),
            Self::I32
                if value.fract() == 0.0 && (i32::MIN as f64..=i32::MAX as f64).contains(&value) =>
            {
                Ok(value as i32 as u32)
            }
            Self::U32 if value.fract() == 0.0 && (0.0..=u32::MAX as f64).contains(&value) => {
                Ok(value as u32)
            }
            Self::Bool if value == 0.0 || value == 1.0 => Ok(value as u32),
            _ => Err(format!("{value} does not fit a {self:?} cell")),
        }
    }

    fn scalar(self) -> VariableType {
        match self {
            Self::F32 => VariableType::F32,
            Self::I32 => VariableType::I32,
            Self::U32 | Self::Bool => VariableType::U32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Access {
    ReadOnly,
    Live,
    /// Tunable while the robot is disarmed; the firmware enforces it on the
    /// framed link, a raw SWD write is not checked
    SafeOnly,
}

impl Access {
    /// `rm_telemetry::Access` discriminant
    pub fn from_tag(tag: u64) -> Option<Self> {
        Some(match tag {
            0 => Self::ReadOnly,
            1 => Self::Live,
            2 => Self::SafeOnly,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    /// FNV-1a of `name`, stable across builds
    pub id: u32,
    pub name: String,
    pub unit: String,
    pub kind: CellKind,
    pub access: Access,
    pub default: f64,
    /// Range and step exist only for tunables
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub max_step: Option<f64>,
    pub requested_address: u64,
    pub applied_address: u64,
}

impl CatalogEntry {
    /// Sample the value the firmware runs with
    pub fn applied_item(&self, id: u32) -> ReadItem {
        self.item(id, self.applied_address)
    }

    pub fn requested_item(&self, id: u32) -> ReadItem {
        self.item(id, self.requested_address)
    }

    fn item(&self, id: u32, address: u64) -> ReadItem {
        ReadItem {
            id,
            address,
            scalar: self.kind.scalar(),
            bit_offset: None,
            bit_size: None,
        }
    }

    /// Bits to write for a request of `value`, checked as the firmware will
    /// check it, so the user hears about a bad value before it is sent.
    pub fn request_bits(&self, value: f64) -> Result<u32, String> {
        if self.access == Access::ReadOnly {
            return Err(format!("{} is read-only", self.name));
        }
        if let (Some(min), Some(max)) = (self.min, self.max) {
            if !(min..=max).contains(&value) {
                return Err(format!("{} takes {min} to {max}", self.name));
            }
        }
        self.kind.encode(value)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub address: u64,
    /// Demangled path of the table static
    pub symbol: String,
    pub entries: Vec<CatalogEntry>,
}

impl Catalog {
    pub fn entry(&self, id: u32) -> Option<&CatalogEntry> {
        self.entries.iter().find(|e| e.id == id)
    }
}

#[derive(Debug, Clone, Copy)]
struct Field {
    offset: u64,
    size: u64,
}

/// Where the table's fields sit, from DWARF.
#[derive(Debug, Clone)]
pub struct TableLayout {
    pub address: u64,
    pub symbol: String,
    magic: Field,
    version: Field,
    entries_ptr: Field,
    entries_len: Field,
    pointer: u64,
    id: Field,
    kind: Field,
    access: Field,
    name_ptr: Field,
    name_len: Field,
    unit_ptr: Field,
    unit_len: Field,
    default: Field,
    min: Field,
    max: Field,
    max_step: Field,
    requested: Field,
    applied: Field,
}

fn member(ty: &TypeHandle, name: &str) -> Result<(Field, TypeHandle), CatalogError> {
    let ty = ty.underlying();
    let found = ty
        .members()
        .and_then(|ms| ms.iter().find(|m| m.name == name))
        .ok_or_else(|| CatalogError::Layout(format!("{} has no field `{name}`", ty.type_name())))?;
    let member_ty = ty.member_type(found);
    let size = member_ty
        .size()
        .ok_or_else(|| CatalogError::Layout(format!("`{name}` has no size")))?;
    Ok((
        Field {
            offset: found.offset,
            size,
        },
        member_ty,
    ))
}

fn scalar(ty: &TypeHandle, name: &str, size: u64) -> Result<Field, CatalogError> {
    let (field, _) = member(ty, name)?;
    if field.size != size {
        return Err(CatalogError::Layout(format!(
            "`{name}` is {} bytes, expected {size}",
            field.size
        )));
    }
    Ok(field)
}

/// A `&str` or `&[T]` field: its pointer and length, absolute within the parent
fn fat(ty: &TypeHandle, name: &str) -> Result<(Field, Field, TypeHandle), CatalogError> {
    let (outer, fat_ty) = member(ty, name)?;
    let (ptr, ptr_ty) = member(&fat_ty, "data_ptr")?;
    let (len, _) = member(&fat_ty, "length")?;
    let shift = |f: Field| Field {
        offset: outer.offset + f.offset,
        size: f.size,
    };
    Ok((shift(ptr), shift(len), ptr_ty))
}

impl TableLayout {
    /// The firmware's table, or `None` when the build declares none.
    pub fn find(elf: &ElfInfo) -> Result<Option<Self>, CatalogError> {
        let table = elf.type_table();
        let candidate = elf.get_variables().into_iter().find_map(|symbol| {
            let ty = symbol.type_handle(table)?.underlying();
            let is_table = ty.members().is_some_and(|ms| {
                ["magic", "version", "entries"]
                    .iter()
                    .all(|want| ms.iter().any(|m| m.name == *want))
            }) && ty.type_name().rsplit("::").next() == Some("Table");
            is_table.then_some((symbol, ty))
        });
        let Some((symbol, ty)) = candidate else {
            return Ok(None);
        };

        let (entries_ptr, entries_len, element_ptr) = fat(&ty, "entries")?;
        // `entries: &[&Entry]`: data_ptr points at `&Entry`, which points at `Entry`
        let entry_ref = element_ptr
            .pointee()
            .ok_or_else(|| CatalogError::Layout("entries is not a slice".into()))?;
        let pointer = entry_ref.size().unwrap_or(4);
        let entry = entry_ref
            .pointee()
            .ok_or_else(|| CatalogError::Layout("entries do not hold references".into()))?
            .underlying();

        let (name_ptr, name_len, _) = fat(&entry, "name")?;
        let (unit_ptr, unit_len, _) = fat(&entry, "unit")?;
        Ok(Some(Self {
            address: symbol.address,
            symbol: symbol.demangled_name.clone(),
            magic: scalar(&ty, "magic", 4)?,
            version: scalar(&ty, "version", 4)?,
            entries_ptr,
            entries_len,
            pointer,
            id: scalar(&entry, "id", 4)?,
            kind: scalar(&entry, "kind", 1)?,
            access: scalar(&entry, "access", 1)?,
            name_ptr,
            name_len,
            unit_ptr,
            unit_len,
            default: scalar(&entry, "default", 4)?,
            min: scalar(&entry, "min", 4)?,
            max: scalar(&entry, "max", 4)?,
            max_step: scalar(&entry, "max_step", 4)?,
            requested: scalar(&entry, "requested", 4)?,
            applied: scalar(&entry, "applied", 4)?,
        }))
    }

    /// Decode the table from `memory`: the ELF image or the live target.
    pub fn read(&self, memory: &mut dyn MemoryAccess) -> Result<Catalog, CatalogError> {
        let invalid = |reason: String| CatalogError::Invalid {
            address: self.address,
            reason,
        };
        let magic = read_uint(memory, self.address, self.magic)? as u32;
        if magic != TABLE_MAGIC {
            return Err(invalid(format!("magic is {magic:#010x}")));
        }
        let version = read_uint(memory, self.address, self.version)? as u32;
        if version != TABLE_VERSION {
            return Err(invalid(format!(
                "format version {version}, this tool reads {TABLE_VERSION}"
            )));
        }
        let list = read_uint(memory, self.address, self.entries_ptr)?;
        let count = read_uint(memory, self.address, self.entries_len)?;
        if count > MAX_ENTRIES {
            return Err(invalid(format!("{count} entries")));
        }

        let mut entries = Vec::with_capacity(count as usize);
        for i in 0..count {
            let at = read_uint(
                memory,
                list + i * self.pointer,
                Field {
                    offset: 0,
                    size: self.pointer,
                },
            )?;
            let entry = self.read_entry(memory, at).map_err(|e| match e {
                CatalogError::Invalid { reason, .. } => invalid(format!("entry {i}: {reason}")),
                other => other,
            })?;
            if let Some(first) = entries.iter().find(|e: &&CatalogEntry| e.id == entry.id) {
                return Err(invalid(format!(
                    "`{}` and `{}` share id {:#010x}",
                    first.name, entry.name, entry.id
                )));
            }
            entries.push(entry);
        }
        Ok(Catalog {
            address: self.address,
            symbol: self.symbol.clone(),
            entries,
        })
    }

    fn read_entry(
        &self,
        memory: &mut dyn MemoryAccess,
        at: u64,
    ) -> Result<CatalogEntry, CatalogError> {
        let invalid = |reason: String| CatalogError::Invalid {
            address: self.address,
            reason,
        };
        let kind_tag = read_uint(memory, at, self.kind)? as u8;
        let kind =
            CellKind::from_tag(kind_tag).ok_or_else(|| invalid(format!("kind {kind_tag}")))?;
        let access_tag = read_uint(memory, at, self.access)?;
        let access =
            Access::from_tag(access_tag).ok_or_else(|| invalid(format!("access {access_tag}")))?;
        let name = read_text(memory, at, self.name_ptr, self.name_len)?;
        let unit = read_text(memory, at, self.unit_ptr, self.unit_len)?;
        let float = |memory: &mut dyn MemoryAccess, field| -> Result<Option<f64>, CatalogError> {
            let v = f32::from_bits(read_uint(memory, at, field)? as u32);
            Ok(v.is_finite().then_some(widen(v)))
        };
        let tunable = access != Access::ReadOnly;
        let min = float(memory, self.min)?.filter(|_| tunable);
        let max = float(memory, self.max)?.filter(|_| tunable);
        let max_step = float(memory, self.max_step)?.filter(|_| tunable);
        Ok(CatalogEntry {
            id: read_uint(memory, at, self.id)? as u32,
            name: name.ok_or_else(|| invalid("name is not text".into()))?,
            unit: unit.ok_or_else(|| invalid("unit is not text".into()))?,
            kind,
            access,
            default: kind.decode(read_uint(memory, at, self.default)? as u32),
            min,
            max,
            max_step,
            requested_address: at + self.requested.offset,
            applied_address: at + self.applied.offset,
        })
    }
}

/// The `f64` a person would write for `v`: 0.2, not 0.20000000298023224.
/// Encoding it back as `f32` gives the same bits.
pub fn widen(v: f32) -> f64 {
    if !v.is_finite() {
        return f64::from(v);
    }
    v.to_string().parse().unwrap_or(f64::from(v))
}

fn read_uint(memory: &mut dyn MemoryAccess, base: u64, field: Field) -> Result<u64, CatalogError> {
    let mut buf = [0u8; 8];
    let len = field.size.min(8) as usize;
    memory.read(base + field.offset, &mut buf[..len])?;
    Ok(u64::from_le_bytes(buf))
}

fn read_text(
    memory: &mut dyn MemoryAccess,
    base: u64,
    ptr: Field,
    len: Field,
) -> Result<Option<String>, CatalogError> {
    let at = read_uint(memory, base, ptr)?;
    let n = read_uint(memory, base, len)?;
    if n > MAX_TEXT {
        return Ok(None);
    }
    let mut bytes = vec![0; n as usize];
    if n > 0 {
        memory.read(at, &mut bytes)?;
    }
    Ok(String::from_utf8(bytes).ok())
}

/// Target memory as the ELF initialises it: every allocated section with file
/// contents, at its run address. Writes are refused.
pub struct ElfImage {
    sections: Vec<(u64, Vec<u8>)>,
}

impl ElfImage {
    pub fn parse(elf: &[u8]) -> Result<Self, String> {
        let file = object::File::parse(elf).map_err(|e| e.to_string())?;
        let sections = file
            .sections()
            .filter(|s| {
                !matches!(
                    s.kind(),
                    SectionKind::UninitializedData
                        | SectionKind::UninitializedTls
                        | SectionKind::Metadata
                        | SectionKind::Debug
                        | SectionKind::DebugString
                        | SectionKind::Other
                        | SectionKind::Unknown
                        | SectionKind::Note
                        | SectionKind::Linker
                )
            })
            .filter(|s| s.address() != 0 || s.size() == 0)
            .filter_map(|s| Some((s.address(), s.data().ok()?.to_vec())))
            .collect();
        Ok(Self { sections })
    }
}

impl ElfImage {
    /// Each initialised region as `(run address, contents)`
    pub fn sections(&self) -> impl Iterator<Item = (u64, &[u8])> {
        self.sections
            .iter()
            .map(|(at, data)| (*at, data.as_slice()))
    }
}

impl MemoryAccess for ElfImage {
    fn read(&mut self, address: u64, buf: &mut [u8]) -> studio_carriers::Result<()> {
        let end = address + buf.len() as u64;
        for (start, data) in &self.sections {
            if address >= *start && end <= start + data.len() as u64 {
                let from = (address - start) as usize;
                buf.copy_from_slice(&data[from..from + buf.len()]);
                return Ok(());
            }
        }
        Err(CarrierError::Read {
            address,
            len: buf.len(),
            reason: "not initialised by the ELF".into(),
        })
    }

    fn write(&mut self, address: u64, data: &[u8]) -> studio_carriers::Result<()> {
        Err(CarrierError::Write {
            address,
            len: data.len(),
            reason: "the ELF image is read-only".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_widen_to_their_shortest_decimal_and_back() {
        for v in [0.2f32, 1.0e-7, 5.0e-7, -3.75, 1.0e30, f32::MIN_POSITIVE] {
            assert_eq!((widen(v) as f32).to_bits(), v.to_bits());
        }
        assert_eq!(widen(0.2), 0.2);
    }

    #[test]
    fn cells_encode_what_they_can_hold() {
        assert_eq!(CellKind::F32.encode(1.5), Ok(1.5f32.to_bits()));
        assert_eq!(CellKind::I32.encode(-2.0), Ok(0xffff_fffe));
        assert!(CellKind::I32.encode(0.5).is_err());
        assert!(CellKind::U32.encode(-1.0).is_err());
        assert_eq!(CellKind::Bool.encode(1.0), Ok(1));
        assert!(CellKind::Bool.encode(2.0).is_err());
        assert!(CellKind::F32.encode(f64::NAN).is_err());
        assert_eq!(CellKind::I32.decode(0xffff_fffe), -2.0);
    }

    #[test]
    fn requests_are_checked_against_access_and_range() {
        let mut entry = CatalogEntry {
            id: 1,
            name: "kp".into(),
            unit: String::new(),
            kind: CellKind::F32,
            access: Access::Live,
            default: 1.0,
            min: Some(0.0),
            max: Some(10.0),
            max_step: Some(0.1),
            requested_address: 0x2400_0000,
            applied_address: 0x2400_0004,
        };
        assert_eq!(entry.request_bits(2.0), Ok(2.0f32.to_bits()));
        assert!(entry.request_bits(11.0).is_err());
        entry.access = Access::ReadOnly;
        assert!(entry.request_bits(2.0).is_err());
    }
}
