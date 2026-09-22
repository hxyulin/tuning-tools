//! Browse statics as a tree: roots are variables, children are struct members,
//! array elements and enum variants, expanded one level at a time.
//!
//! Nodes are addressed by [`NodeRef`] (symbol path plus steps), never by address,
//! so a reference stays meaningful after a rebuild moves things around.
//! Pointer children describe types; live addresses require `node_with_memory`.
//!
//! Wrappers that only hold one value (`AtomicU32`, `UnsafeCell`, `Cell`,
//! `MaybeUninit`, embassy's blocking `Mutex`, newtypes) are shown through: a
//! node has the type of the value inside, and its children are that value's.
//! A reference therefore names members of the innermost type; one that names a
//! wrapper's own member still resolves.
//!
//! An embassy task pool is untyped bytes; [`Step::Task`] reads one of its slots
//! as the task's `TaskStorage`, see [`crate::tasks`].

use crate::dwarf_parser::DwarfDiagnostics;
use crate::elf::{ElfInfo, SymbolInfo};
use crate::tasks;
use crate::type_table::{MemberDef, SourceLocation, TypeDef, TypeId, TypeTable, VariantDef};
use crate::variable_type::VariableType;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Array elements returned per [`children`] call when no limit is given.
pub const DEFAULT_CHILD_LIMIT: usize = 256;

/// One step from a node to a child.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum Step {
    Member(String),
    Index(u64),
    /// Payload of a Rust enum variant
    Variant(String),
    /// Tag member of a Rust enum
    Discriminant,
    /// Slot of an embassy task pool, as the task's `TaskStorage`; only the
    /// first step
    Task(u64),
    /// Follow a pointer using current target memory.
    Deref,
    SliceIndex(u64),
}

/// Stable reference to a node: full symbol path, then steps into its type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeRef {
    pub symbol: String,
    pub steps: Vec<Step>,
}

impl NodeRef {
    pub fn root(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            steps: Vec::new(),
        }
    }

    pub fn child(&self, step: Step) -> Self {
        let mut steps = self.steps.clone();
        steps.push(step);
        Self {
            symbol: self.symbol.clone(),
            steps,
        }
    }
}

/// `gimbal::GIMBAL.pitch.limit#Some.__0`, `SAMPLES[3]`, `cmd#<discriminant>`,
/// `app::led_task::POOL[task 0].future`
impl fmt::Display for NodeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.symbol)?;
        for step in &self.steps {
            match step {
                Step::Deref => f.write_str(".*")?,
                Step::Member(name) => write!(f, ".{name}")?,
                Step::SliceIndex(i) => write!(f, "[{i}]")?,
                Step::Index(i) => write!(f, "[{i}]")?,
                Step::Variant(name) => write!(f, "#{name}")?,
                Step::Discriminant => f.write_str("#<discriminant>")?,
                Step::Task(slot) => write!(f, "[task {slot}]")?,
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeKind {
    /// Integer, float or bool
    Scalar,
    /// C-like enum stored as an integer
    Enum,
    /// Rust enum with data (`DW_TAG_variant_part`)
    TaggedEnum,
    Struct,
    Union,
    Array,
    Pointer,
    Function,
    Other,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolNode {
    #[serde(rename = "ref")]
    pub node: NodeRef,
    /// Display label: short symbol name, member name, `[i]` or variant name
    pub label: String,
    /// `node` rendered as a string
    pub path: String,
    pub address: u64,
    pub size: Option<u64>,
    pub type_name: String,
    /// Outermost type when the node shows through wrappers, e.g. `Atomic<u32>`
    pub wrapper: Option<String>,
    pub kind: NodeKind,
    /// How to decode the bytes when the node is a single value
    pub scalar: Option<VariableType>,
    /// Rust str/slice fat pointer with DWARF data_ptr and length fields.
    pub sequence: bool,
    pub expandable: bool,
    /// Members / elements / variants, when known
    pub child_count: Option<u64>,
    pub readable: bool,
    /// Why the node cannot be read, when it cannot
    pub status: Option<String>,
    pub bit_offset: Option<u64>,
    pub bit_size: Option<u64>,
    /// For variant nodes: the tag value selecting this variant (`None` = default)
    pub discr_value: Option<u64>,
    /// For variant nodes: where the variant is declared; an `async fn` suspend
    /// variant's is its `.await`
    pub location: Option<SourceLocation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootNode {
    #[serde(flatten)]
    pub node: SymbolNode,
    /// Path split on `::`, respecting `<...>` (`<A as B>::info::INFO` has 3 segments)
    pub segments: Vec<String>,
    pub section: String,
    /// Not in an allocated, writable section: flash constants, vtables, defmt strings
    pub read_only: bool,
    /// Owned by the runtime, not the application: task storage, RTT buffers,
    /// embassy and defmt state
    pub internal: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Children {
    pub nodes: Vec<SymbolNode>,
    /// Total children; larger than `nodes.len()` when an array was truncated
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ElfSummary {
    pub path: String,
    pub machine: String,
    pub is_64bit: bool,
    pub little_endian: bool,
    pub entry_point: u64,
    pub variables: usize,
    pub functions: usize,
    pub types: usize,
    pub diagnostics: DiagnosticsSummary,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsSummary {
    pub total_variables: usize,
    pub with_valid_address: usize,
    pub optimized_out: usize,
    pub local_variables: usize,
    pub extern_declarations: usize,
    pub compile_time_constants: usize,
    pub register_only: usize,
}

impl From<&DwarfDiagnostics> for DiagnosticsSummary {
    fn from(d: &DwarfDiagnostics) -> Self {
        Self {
            total_variables: d.total_variables,
            with_valid_address: d.with_valid_address,
            optimized_out: d.optimized_out,
            local_variables: d.local_variables,
            extern_declarations: d.extern_declarations,
            compile_time_constants: d.compile_time_constants,
            register_only: d.register_only,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    #[error("symbol not found: {0}")]
    SymbolNotFound(String),
    #[error("{symbol} has no type information")]
    Untyped { symbol: String },
    #[error("cannot step into {at}: {reason}")]
    BadStep { at: String, reason: String },
}

pub fn summary(elf: &ElfInfo) -> ElfSummary {
    ElfSummary {
        path: elf.path.clone(),
        machine: elf.machine.clone(),
        is_64bit: elf.is_64bit,
        little_endian: elf.is_little_endian,
        entry_point: elf.entry_point,
        variables: elf.variable_count(),
        functions: elf.function_count(),
        types: elf.type_table().len(),
        diagnostics: elf.get_diagnostics().into(),
    }
}

/// Every typed variable symbol, sorted by path.
pub fn roots(elf: &ElfInfo) -> Vec<RootNode> {
    let table = elf.type_table();
    let mut roots: Vec<RootNode> = elf
        .get_variables()
        .into_iter()
        .filter_map(|sym| {
            let type_id = sym.type_id?;
            let mut node = describe(
                table,
                NodeRef::root(&sym.demangled_name),
                sym.display_name.clone(),
                sym.address,
                type_id,
            );
            if node.size.is_none() && sym.size > 0 {
                node.size = Some(sym.size);
            }
            node.readable = sym.is_readable();
            node.status = sym.unreadable_reason().map(str::to_string);
            Some(RootNode {
                segments: split_path(&sym.demangled_name),
                read_only: !sym.writable,
                internal: is_internal(&sym.demangled_name, &sym.section, &table.type_name(type_id)),
                section: sym.section.clone(),
                node,
            })
        })
        .collect();
    roots.sort_by(|a, b| a.node.path.cmp(&b.node.path));
    roots.dedup_by(|a, b| a.node.path == b.node.path && a.node.address == b.node.address);
    roots
}

/// Describe the node `node` refers to.
pub fn node(elf: &ElfInfo, node: &NodeRef) -> Result<SymbolNode, TreeError> {
    node_impl(
        elf,
        node,
        &mut |_, _| Err("pointer following requires live memory".into()),
        false,
    )
}

/// Resolve a node with a caller-owned pointer reader. Never caches live addresses.
pub fn node_with_memory(
    elf: &ElfInfo,
    node: &NodeRef,
    read_pointer: &mut dyn FnMut(u64, usize) -> Result<u64, String>,
) -> Result<SymbolNode, TreeError> {
    node_impl(elf, node, read_pointer, true)
}

pub fn metadata_node(elf: &ElfInfo, node: &NodeRef) -> Result<SymbolNode, TreeError> {
    node_impl(elf, node, &mut |_, _| Ok(0), false)
}

fn node_impl(
    elf: &ElfInfo,
    node: &NodeRef,
    read_pointer: &mut dyn FnMut(u64, usize) -> Result<u64, String>,
    live: bool,
) -> Result<SymbolNode, TreeError> {
    let (sym, at) = resolve_with_memory(elf, node, read_pointer, live)?;
    let mut out = describe(
        elf.type_table(),
        node.clone(),
        at.label,
        at.address,
        at.type_id,
    );
    out.bit_offset = at.bit_offset;
    out.bit_size = at.bit_size;
    out.discr_value = at.discr_value;
    if !sym.is_readable() {
        out.readable = false;
        out.status = sym.unreadable_reason().map(str::to_string);
    }
    Ok(out)
}

/// Children of `node`, at most `limit` array elements (default [`DEFAULT_CHILD_LIMIT`]).
pub fn children(
    elf: &ElfInfo,
    node: &NodeRef,
    limit: Option<usize>,
) -> Result<Children, TreeError> {
    children_page(
        elf,
        node,
        0,
        limit.unwrap_or(DEFAULT_CHILD_LIMIT),
        &mut |_, _| Ok(0),
        false,
    )
}

pub fn children_page(
    elf: &ElfInfo,
    node: &NodeRef,
    offset: u64,
    limit: usize,
    reader: &mut dyn FnMut(u64, usize) -> Result<u64, String>,
    live: bool,
) -> Result<Children, TreeError> {
    let table = elf.type_table();
    let (sym, at) = resolve_with_memory(elf, node, reader, live)?;
    let limit = limit.min(4096) as u64;
    if let Some((data, length, element, _)) = sequence(table, at.type_id).filter(|_| live) {
        let bad = |reason| TreeError::BadStep {
            at: node.to_string(),
            reason,
        };
        let count =
            reader(at.address + length.offset, if elf.is_64bit { 8 } else { 4 }).map_err(bad)?;
        let pointer =
            reader(at.address + data.offset, if elf.is_64bit { 8 } else { 4 }).map_err(bad)?;
        let stride = table
            .type_size(element)
            .ok_or_else(|| bad("unknown slice element size".into()))?;
        let mut nodes = Vec::new();
        for i in offset.min(count)..offset.saturating_add(limit).min(count) {
            let address = pointer
                .checked_add(
                    i.checked_mul(stride)
                        .ok_or_else(|| bad("slice offset overflow".into()))?,
                )
                .ok_or_else(|| bad("slice address overflow".into()))?;
            nodes.push(describe(
                table,
                node.child(Step::SliceIndex(i)),
                format!("[{i}]"),
                address,
                element,
            ));
        }
        return Ok(Children {
            nodes,
            total: count,
        });
    }
    let readable = sym.is_readable();

    let member_node = |step: Step, m: &MemberDef, discr_value: Option<u64>| {
        let label = match &step {
            Step::Discriminant => "<discriminant>".to_string(),
            _ => m.name.clone(),
        };
        let mut n = describe(
            table,
            node.child(step),
            label,
            at.address + m.offset,
            m.type_id,
        );
        n.bit_offset = m.bit_offset;
        n.bit_size = m.bit_size;
        n.discr_value = discr_value;
        n.readable = readable;
        n
    };

    let nodes: Vec<SymbolNode> = match table.get(table.get_underlying(peel(table, at.type_id))) {
        Some(TypeDef::Struct(s)) | Some(TypeDef::Union(s)) => match &s.variant_part {
            Some(part) => part
                .discriminant
                .iter()
                .map(|d| member_node(Step::Discriminant, d, None))
                .chain(part.variants.iter().map(|v| {
                    let mut n = member_node(
                        Step::Variant(v.member.name.clone()),
                        &v.member,
                        v.discr_value,
                    );
                    n.label = variant_label(table, v);
                    n.location = v.location.clone();
                    n
                }))
                .collect(),
            None => s
                .members
                .iter()
                .map(|m| member_node(Step::Member(m.name.clone()), m, None))
                .collect(),
        },
        Some(TypeDef::Pointer(inner) | TypeDef::Reference(inner)) => {
            vec![describe(
                table,
                node.child(Step::Deref),
                "*".into(),
                0,
                *inner,
            )]
        }
        Some(TypeDef::Array {
            element,
            count: Some(count),
        }) => {
            let elem_size = table.type_size(*element).unwrap_or(0);
            let shown = (*count).min(offset.saturating_add(limit));
            let nodes = (offset.min(*count)..shown)
                .map(|i| {
                    let mut n = describe(
                        table,
                        node.child(Step::Index(i)),
                        format!("[{i}]"),
                        at.address + i * elem_size,
                        *element,
                    );
                    n.readable = readable;
                    n
                })
                .collect();
            return Ok(Children {
                nodes,
                total: *count,
            });
        }
        _ => Vec::new(),
    };
    let total = nodes.len() as u64;
    Ok(Children { nodes, total })
}

struct Resolved {
    label: String,
    address: u64,
    type_id: TypeId,
    bit_offset: Option<u64>,
    bit_size: Option<u64>,
    discr_value: Option<u64>,
}

fn resolve_with_memory<'e>(
    elf: &'e ElfInfo,
    node: &NodeRef,
    read_pointer: &mut dyn FnMut(u64, usize) -> Result<u64, String>,
    live: bool,
) -> Result<(&'e SymbolInfo, Resolved), TreeError> {
    if node
        .steps
        .iter()
        .filter(|s| matches!(s, Step::Deref | Step::SliceIndex(_)))
        .count()
        > 8
    {
        return Err(TreeError::BadStep {
            at: node.to_string(),
            reason: "pointer depth limit (8) reached".into(),
        });
    }
    let table = elf.type_table();
    let sym = elf
        .find_symbol(&node.symbol)
        .ok_or_else(|| TreeError::SymbolNotFound(node.symbol.clone()))?;
    if live && !sym.is_readable() {
        return Err(TreeError::BadStep {
            at: node.to_string(),
            reason: "not readable".into(),
        });
    }
    let mut at = Resolved {
        label: sym.display_name.clone(),
        address: sym.address,
        type_id: sym.type_id.ok_or_else(|| TreeError::Untyped {
            symbol: node.symbol.clone(),
        })?,
        bit_offset: None,
        bit_size: None,
        discr_value: None,
    };

    let mut walked = NodeRef::root(&node.symbol);
    for step in &node.steps {
        if let Step::Task(slot) = step {
            at = enter_task(table, sym, &walked, *slot)?;
            walked = walked.child(step.clone());
            continue;
        }
        let bad = |reason| TreeError::BadStep {
            at: walked.to_string(),
            reason,
        };
        if let Step::SliceIndex(i) = step {
            let (data, length, element, _) =
                sequence(table, at.type_id).ok_or_else(|| bad("not a slice".into()))?;
            let width = if elf.is_64bit { 8 } else { 4 };
            let count = read_pointer(at.address + length.offset, width).map_err(bad)?;
            if live && *i >= count {
                return Err(bad("slice index out of bounds".into()));
            }
            let pointer = read_pointer(at.address + data.offset, width).map_err(bad)?;
            if live && pointer == 0 {
                return Err(bad("null slice pointer".into()));
            }
            let stride = table
                .type_size(element)
                .ok_or_else(|| bad("unknown element size".into()))?;
            let address = pointer
                .checked_add(
                    i.checked_mul(stride)
                        .ok_or_else(|| bad("slice offset overflow".into()))?,
                )
                .ok_or_else(|| bad("slice address overflow".into()))?;
            at = Resolved {
                label: format!("[{i}]"),
                address,
                type_id: element,
                bit_offset: None,
                bit_size: None,
                discr_value: None,
            };
            walked = walked.child(step.clone());
            continue;
        }
        if live {
            if let Step::Variant(name) = step {
                if let Some(TypeDef::Struct(s)) =
                    table.get(table.get_underlying(peel(table, at.type_id)))
                {
                    if let Some(part) = &s.variant_part {
                        let tag = if let Some(d) = &part.discriminant {
                            read_pointer(
                                at.address + d.offset,
                                table.type_size(d.type_id).unwrap_or(0) as usize,
                            )
                            .map_err(bad)?
                        } else {
                            0
                        };
                        if part.select(tag).is_none_or(|v| v.member.name != *name) {
                            return Err(bad("inactive variant".into()));
                        }
                    }
                }
            }
        }
        if matches!(step, Step::Deref) {
            let bad = |reason| TreeError::BadStep {
                at: walked.to_string(),
                reason,
            };
            if !sym.is_readable() {
                return Err(bad("not readable".into()));
            }
            let Some(TypeDef::Pointer(inner) | TypeDef::Reference(inner)) =
                table.get(table.get_underlying(peel(table, at.type_id)))
            else {
                return Err(bad("not a pointer".into()));
            };
            let address =
                read_pointer(at.address, if elf.is_64bit { 8 } else { 4 }).map_err(bad)?;
            if live && address == 0 {
                return Err(bad("null pointer".into()));
            }
            at = Resolved {
                label: "*".into(),
                address,
                type_id: *inner,
                bit_offset: None,
                bit_size: None,
                discr_value: None,
            };
            walked = walked.child(step.clone());
            continue;
        }
        // Innermost first: that is what the tree hands out. Outer levels match
        // references that step through a wrapper's own members.
        let mut tried = wrapper_chain(table, at.type_id)
            .into_iter()
            .rev()
            .map(|t| enter(table, t, at.address, step));
        let innermost = tried
            .next()
            .expect("a chain holds at least the type itself");
        at = match innermost {
            Ok(next) => next,
            Err(reason) => tried
                .find_map(Result::ok)
                .ok_or_else(|| TreeError::BadStep {
                    at: walked.to_string(),
                    reason,
                })?,
        };
        walked = walked.child(step.clone());
    }
    Ok((sym, at))
}

/// Slot `slot` of the task pool `sym`, typed as the task's storage.
fn enter_task(
    table: &TypeTable,
    sym: &SymbolInfo,
    walked: &NodeRef,
    slot: u64,
) -> Result<Resolved, TreeError> {
    let bad = |reason: String| TreeError::BadStep {
        at: walked.to_string(),
        reason,
    };
    if !walked.steps.is_empty() {
        return Err(bad("only a task pool has task slots".into()));
    }
    let storage = tasks::storage_type(table, &sym.demangled_name, sym.size)
        .ok_or_else(|| bad("not an embassy task pool with a known task type".into()))?;
    if slot >= storage.slots {
        return Err(bad(format!(
            "task slot {slot} out of bounds; the pool holds {}",
            storage.slots
        )));
    }
    Ok(Resolved {
        label: format!("[task {slot}]"),
        address: sym.address + slot * storage.size,
        type_id: storage.type_id,
        bit_offset: None,
        bit_size: None,
        discr_value: None,
    })
}

/// A variant's name. Coroutine variants are members named by number (`3`)
/// with a payload type named after the state (`…::Suspend0`).
fn variant_label(table: &TypeTable, v: &VariantDef) -> String {
    let name = &v.member.name;
    if !name.bytes().all(|b| b.is_ascii_digit()) {
        return name.clone();
    }
    match table
        .get(table.get_underlying(v.member.type_id))
        .and_then(TypeDef::name)
    {
        Some(payload) => split_path(payload).pop().unwrap_or_else(|| name.clone()),
        None => name.clone(),
    }
}

/// Take `step` from a value of type `type_id` at `address`.
fn enter(
    table: &TypeTable,
    type_id: TypeId,
    address: u64,
    step: &Step,
) -> Result<Resolved, String> {
    let def = table.get(table.get_underlying(type_id));
    let (member, discr_value) = match (step, def) {
        (Step::Member(name), Some(TypeDef::Struct(s) | TypeDef::Union(s))) => (
            s.members
                .iter()
                .find(|m| &m.name == name)
                .ok_or_else(|| format!("no member `{name}`"))?,
            None,
        ),
        (Step::Variant(name), Some(TypeDef::Struct(s))) => {
            let v = s
                .variant_part
                .as_ref()
                .and_then(|p| p.variants.iter().find(|v| &v.member.name == name))
                .ok_or_else(|| format!("no variant `{name}`"))?;
            (&v.member, v.discr_value)
        }
        (Step::Discriminant, Some(TypeDef::Struct(s))) => (
            s.variant_part
                .as_ref()
                .and_then(|p| p.discriminant.as_ref())
                .ok_or("not a tagged enum")?,
            None,
        ),
        (Step::Task(_), _) => return Err("only a task pool has task slots".to_string()),
        (Step::Index(i), Some(TypeDef::Array { element, count })) => {
            if count.is_some_and(|c| *i >= c) {
                return Err(format!("index {i} out of bounds"));
            }
            let elem_size = table.type_size(*element).unwrap_or(0);
            return Ok(Resolved {
                label: format!("[{i}]"),
                address: address
                    .checked_add(i.checked_mul(elem_size).ok_or("array offset overflow")?)
                    .ok_or("address overflow")?,
                type_id: *element,
                bit_offset: None,
                bit_size: None,
                discr_value: None,
            });
        }
        _ => return Err("step does not match the type".to_string()),
    };
    Ok(Resolved {
        label: match step {
            Step::Discriminant => "<discriminant>".to_string(),
            _ => member.name.clone(),
        },
        address: address
            .checked_add(member.offset)
            .ok_or("address overflow")?,
        type_id: member.type_id,
        bit_offset: member.bit_offset,
        bit_size: member.bit_size,
        discr_value,
    })
}

/// Deepest a wrapper chain is followed, against a cyclic type table.
const MAX_WRAPPERS: usize = 16;

/// The one value a wrapper holds: its only member that is not zero-sized,
/// filling the whole wrapper. `Atomic<u32>.v`, `Mutex.data` (beside a
/// zero-sized `raw`), `MaybeUninit.value`. Tagged enums are not wrappers.
fn held_member(table: &TypeTable, type_id: TypeId) -> Option<&MemberDef> {
    let (Some(TypeDef::Struct(s)) | Some(TypeDef::Union(s))) =
        table.get(table.get_underlying(type_id))
    else {
        return None;
    };
    if s.variant_part.is_some() || !s.base_classes.is_empty() {
        return None;
    }
    let mut held = None;
    for m in &s.members {
        match table.type_size(m.type_id) {
            Some(0) => {}
            Some(size)
                if size == s.size && m.offset == 0 && m.bit_size.is_none() && held.is_none() =>
            {
                held = Some(m)
            }
            _ => return None,
        }
    }
    held
}

/// `type_id`, then each type it wraps, outermost first.
fn wrapper_chain(table: &TypeTable, type_id: TypeId) -> Vec<TypeId> {
    let mut chain = vec![type_id];
    while chain.len() <= MAX_WRAPPERS {
        match held_member(table, chain[chain.len() - 1]) {
            Some(m) => chain.push(m.type_id),
            None => break,
        }
    }
    chain
}

/// The value inside every wrapper around `type_id`.
fn peel(table: &TypeTable, type_id: TypeId) -> TypeId {
    wrapper_chain(table, type_id)
        .pop()
        .expect("a chain holds at least the type itself")
}

/// Crates whose statics are runtime plumbing rather than application state.
const RUNTIME_CRATES: [&str; 6] = [
    "embassy_",
    "defmt",
    "rtt_target",
    "cortex_m",
    "critical_section",
    "static_cell",
];

/// Task storage, cells handing out buffers once, RTT, and runtime crates' own
/// statics. Judged by the outer type name, before wrappers are shown through.
fn is_internal(path: &str, section: &str, type_name: &str) -> bool {
    let krate = path.trim_start_matches('<');
    section == ".rtt"
        || path == "_SEGGER_RTT"
        || type_name.starts_with("TaskPoolHolder<")
        || type_name.starts_with("StaticCell<")
        || RUNTIME_CRATES.iter().any(|c| krate.starts_with(c))
}

pub(crate) fn describe(
    table: &TypeTable,
    node: NodeRef,
    label: String,
    address: u64,
    outer: TypeId,
) -> SymbolNode {
    let type_id = peel(table, outer);
    let underlying = table.get(table.get_underlying(type_id));
    let (kind, child_count) = match underlying {
        Some(TypeDef::Primitive(_)) => (NodeKind::Scalar, None),
        Some(TypeDef::Enum(_)) => (NodeKind::Enum, None),
        Some(TypeDef::Struct(s)) => match &s.variant_part {
            Some(p) => (
                NodeKind::TaggedEnum,
                Some((p.variants.len() + usize::from(p.discriminant.is_some())) as u64),
            ),
            None => (NodeKind::Struct, Some(s.members.len() as u64)),
        },
        Some(TypeDef::Union(s)) => (NodeKind::Union, Some(s.members.len() as u64)),
        Some(TypeDef::Array { count, .. }) => (NodeKind::Array, *count),
        Some(TypeDef::Pointer(inner) | TypeDef::Reference(inner)) => (
            NodeKind::Pointer,
            table.type_size(*inner).filter(|size| *size > 0).map(|_| 1),
        ),
        Some(TypeDef::Subroutine { .. }) => (NodeKind::Function, None),
        _ => (NodeKind::Other, None),
    };
    let scalar = matches!(kind, NodeKind::Scalar | NodeKind::Enum | NodeKind::Pointer)
        .then(|| table.to_variable_type(table.get_underlying(type_id)));
    SymbolNode {
        path: node.to_string(),
        node,
        label,
        address,
        size: table.type_size(type_id),
        type_name: table.type_name(type_id),
        wrapper: (type_id != outer).then(|| table.type_name(outer)),
        kind,
        scalar,
        sequence: sequence(table, outer).is_some(),
        expandable: sequence(table, outer).is_some() || child_count.is_some_and(|c| c > 0),
        child_count,
        readable: true,
        status: None,
        bit_offset: None,
        bit_size: None,
        discr_value: None,
        location: None,
    }
}

/// Split a demangled path on `::` outside `<...>`, `(...)` and `[...]`.
pub fn split_path(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' | b')' | b']' => depth -= 1,
            b':' if depth == 0 && bytes.get(i + 1) == Some(&b':') => {
                segments.push(path[start..i].to_string());
                i += 2;
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    segments.push(path[start..].to_string());
    segments
}

/// Recognize only genuine Rust borrowed str/slice DWARF layouts, not arbitrary structs.
fn sequence(table: &TypeTable, outer: TypeId) -> Option<(&MemberDef, &MemberDef, TypeId, bool)> {
    let Some(TypeDef::Struct(s)) = table.get(table.get_underlying(peel(table, outer))) else {
        return None;
    };
    let name = s.name.as_deref()?;
    let is_str = name == "&str" || name == "&mut str";
    if !is_str && !(name.starts_with("&[") || name.starts_with("&mut [")) {
        return None;
    }
    let data = s.members.iter().find(|m| m.name == "data_ptr")?;
    let length = s.members.iter().find(|m| m.name == "length")?;
    let Some(TypeDef::Pointer(element)) = table.get(table.get_underlying(data.type_id)) else {
        return None;
    };
    Some((data, length, *element, is_str))
}

pub struct Preview {
    pub text: String,
    pub active_variant: Option<String>,
}
/// Bounded string/slice previews and enum labels, with inactive payload checks.
pub fn preview(
    elf: &ElfInfo,
    node: &NodeRef,
    read: &mut dyn FnMut(u64, usize) -> Result<Vec<u8>, String>,
) -> Result<Option<Preview>, String> {
    let word = |bytes: Vec<u8>| -> Result<u64, String> {
        if bytes.is_empty() || bytes.len() > 8 {
            return Err("unsupported word size".into());
        }
        let mut raw = [0u8; 8];
        if elf.is_little_endian {
            raw[..bytes.len()].copy_from_slice(&bytes);
            Ok(u64::from_le_bytes(raw))
        } else {
            raw[8 - bytes.len()..].copy_from_slice(&bytes);
            Ok(u64::from_be_bytes(raw))
        }
    };
    let (sym, at) = resolve_with_memory(elf, node, &mut |a, n| word(read(a, n)?), true)
        .map_err(|e| e.to_string())?;
    if !sym.is_readable() {
        return Err("not readable".into());
    }
    let table = elf.type_table();
    let mut active_variant = None;
    let text = if let Some((data, length, _, is_str)) = sequence(table, at.type_id) {
        let width = if elf.is_64bit { 8 } else { 4 };
        let count = word(read(at.address + length.offset, width)?)?;
        if is_str {
            let pointer = word(read(at.address + data.offset, width)?)?;
            if count > 0 && pointer == 0 {
                return Err("null string pointer".into());
            }
            let bytes = if count == 0 {
                vec![]
            } else {
                read(pointer, count.min(256) as usize)?
            };
            let text = String::from_utf8_lossy(&bytes);
            format!("{text:?}{}", if count > 256 { "… (truncated)" } else { "" })
        } else {
            format!("[{count} elements]")
        }
    } else {
        match table.get(table.get_underlying(peel(table, at.type_id))) {
            Some(TypeDef::Enum(e)) => {
                let value = word(read(at.address, e.size as usize)?)?;
                // Enumerators are signed i64 in DWARF; match both raw bits and sign extension.
                let bits = e.size * 8;
                let signed = if bits > 0 && bits < 64 {
                    ((value << (64 - bits)) as i64) >> (64 - bits)
                } else {
                    value as i64
                };
                e.variants
                    .iter()
                    .find(|v| v.value as u64 == value || v.value == signed)
                    .map(|v| format!("{} ({value})", v.name))
                    .unwrap_or_else(|| value.to_string())
            }
            Some(TypeDef::Struct(s)) if s.variant_part.is_some() => {
                let part = s.variant_part.as_ref().unwrap();
                let tag = if let Some(d) = &part.discriminant {
                    word(read(
                        at.address + d.offset,
                        table.type_size(d.type_id).unwrap_or(0) as usize,
                    )?)?
                } else {
                    0
                };
                let variant = part.select(tag).ok_or("unknown enum discriminant")?;
                active_variant = Some(variant.member.name.clone());
                variant_label(table, variant)
            }
            Some(TypeDef::Primitive(crate::type_table::PrimitiveDef::UnicodeChar { size })) => {
                let value = word(read(at.address, *size as usize)?)?;
                char::from_u32(u32::try_from(value).map_err(|_| "invalid character")?)
                    .map(|c| format!("{c:?} (U+{value:04X})"))
                    .ok_or("invalid character")?
            }
            _ => return Ok(None),
        }
    };
    Ok(Some(Preview {
        text,
        active_variant,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::ElfParser;

    const RUST_V0_ELF: &[u8] = include_bytes!("../tests/fixtures/rust_v0.elf");
    const TEST_STRUCT_ELF: &[u8] = include_bytes!("../tests/fixtures/test_struct.elf");
    const GIMBAL: &str = "rust_fixture::control::GIMBAL";

    fn rust() -> ElfInfo {
        ElfParser::parse_bytes(RUST_V0_ELF, "rust_v0.elf").unwrap()
    }

    /// `GIMBAL` is `Shared<Gimbal<4>>`, a newtype over `UnsafeCell`
    fn gimbal_value() -> NodeRef {
        NodeRef::root(GIMBAL)
    }

    fn labels(c: &Children) -> Vec<&str> {
        c.nodes.iter().map(|n| n.label.as_str()).collect()
    }

    #[test]
    fn split_path_respects_generics_and_qualified_paths() {
        assert_eq!(split_path("a::b::C"), ["a", "b", "C"]);
        assert_eq!(split_path("C_COUNTER"), ["C_COUNTER"]);
        assert_eq!(
            split_path("<embassy_stm32::usart::UART5 as embassy_stm32::usart::SealedInstance>::state::STATE"),
            [
                "<embassy_stm32::usart::UART5 as embassy_stm32::usart::SealedInstance>",
                "state",
                "STATE"
            ]
        );
        assert_eq!(
            split_path("m::f::<u8, [u8; 2]>::X"),
            ["m", "f", "<u8, [u8; 2]>", "X"]
        );
    }

    #[test]
    fn roots_list_typed_statics_with_segments() {
        let elf = rust();
        let roots = roots(&elf);
        let samples = roots
            .iter()
            .find(|r| r.node.path == "rust_fixture::telemetry::SAMPLES")
            .unwrap();
        assert_eq!(samples.segments, ["rust_fixture", "telemetry", "SAMPLES"]);
        assert_eq!(samples.node.label, "SAMPLES");
        assert_eq!(samples.node.kind, NodeKind::Array);
        assert_eq!(samples.node.child_count, Some(8));
        assert!(!samples.read_only);

        let reset = roots
            .iter()
            .find(|r| r.node.label == "RESET_VECTOR")
            .unwrap();
        assert!(reset.read_only, "section {}", reset.section);
        assert_eq!(reset.node.kind, NodeKind::Pointer);
    }

    #[test]
    fn walk_struct_members_down_to_scalars() {
        let elf = rust();
        let sym_addr = elf.find_symbol(GIMBAL).unwrap().address;
        let fields = children(&elf, &gimbal_value(), None).unwrap();
        let mut sorted = labels(&fields);
        sorted.sort();
        assert_eq!(
            sorted,
            [
                "command",
                "history",
                "last_fault",
                "mode",
                "offset",
                "pitch",
                "yaw"
            ]
        );

        let kp_ref = gimbal_value()
            .child(Step::Member("yaw".into()))
            .child(Step::Member("kp".into()));
        let kp = node(&elf, &kp_ref).unwrap();
        assert_eq!(kp.path, "rust_fixture::control::GIMBAL.yaw.kp");
        assert_eq!(kp.kind, NodeKind::Scalar);
        assert_eq!(kp.scalar, Some(VariableType::F32));
        assert_eq!(kp.address, sym_addr + 28);
        assert!(!kp.expandable);

        let mode = node(&elf, &gimbal_value().child(Step::Member("mode".into()))).unwrap();
        assert_eq!(mode.kind, NodeKind::Enum);
        assert_eq!(mode.scalar, Some(VariableType::U8));
    }

    #[test]
    fn arrays_index_and_truncate() {
        let elf = rust();
        let samples = NodeRef::root("rust_fixture::telemetry::SAMPLES");
        let base = elf.find_symbol(&samples.symbol).unwrap().address;

        let all = children(&elf, &samples, None).unwrap();
        assert_eq!(all.total, 8);
        assert_eq!(all.nodes[3].path, "rust_fixture::telemetry::SAMPLES[3]");
        assert_eq!(all.nodes[3].address, base + 6);
        assert_eq!(all.nodes[3].type_name, "u16");

        let some = children(&elf, &samples, Some(2)).unwrap();
        assert_eq!((some.nodes.len(), some.total), (2, 8));

        let out = node(&elf, &samples.child(Step::Index(8)));
        assert!(matches!(out, Err(TreeError::BadStep { .. })), "{out:?}");
    }

    #[test]
    fn tagged_enum_children_are_discriminant_then_variants() {
        let elf = rust();
        let command = gimbal_value().child(Step::Member("command".into()));
        let c = node(&elf, &command).unwrap();
        assert_eq!(c.kind, NodeKind::TaggedEnum);
        assert_eq!(c.child_count, Some(4));

        let kids = children(&elf, &command, None).unwrap();
        assert_eq!(
            labels(&kids),
            ["<discriminant>", "Stop", "Velocity", "Position"]
        );
        assert_eq!(kids.nodes[0].scalar, Some(VariableType::U32));
        assert_eq!(kids.nodes[0].path, format!("{command}#<discriminant>"));
        assert_eq!(kids.nodes[3].discr_value, Some(2));

        let target = command
            .child(Step::Variant("Position".into()))
            .child(Step::Member("target".into()));
        let t = node(&elf, &target).unwrap();
        assert_eq!(t.path, format!("{command}#Position.target"));
        assert_eq!(t.address, c.address + 4);
        assert_eq!(t.scalar, Some(VariableType::F32));
    }

    #[test]
    fn bad_references_are_errors() {
        let elf = rust();
        assert_eq!(
            node(&elf, &NodeRef::root("nope")).unwrap_err(),
            TreeError::SymbolNotFound("nope".into())
        );
        let err = node(&elf, &gimbal_value().child(Step::Member("roll".into()))).unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot step into rust_fixture::control::GIMBAL: no member `roll`"
        );
    }

    #[test]
    fn wrappers_are_shown_through() {
        let elf = rust();
        let roots = roots(&elf);
        let root = |path: &str| roots.iter().find(|r| r.node.path == path).unwrap();

        let frames = root("rust_fixture::telemetry::TX_FRAMES");
        assert_eq!(frames.node.kind, NodeKind::Scalar);
        assert_eq!(frames.node.scalar, Some(VariableType::U32));
        assert_eq!(frames.node.type_name, "u32");
        assert!(frames.node.wrapper.as_deref().unwrap().contains("Atomic"));
        assert!(!frames.node.expandable);

        let gimbal = root(GIMBAL);
        assert_eq!(gimbal.node.type_name, "Gimbal<4>");
        assert!(gimbal
            .node
            .wrapper
            .as_deref()
            .unwrap()
            .starts_with("Shared<"));

        // A plain struct has no wrapper
        let yaw = node(&elf, &gimbal_value().child(Step::Member("yaw".into()))).unwrap();
        assert_eq!(yaw.wrapper, None);
    }

    #[test]
    fn references_through_a_wrapper_still_resolve() {
        let elf = rust();
        let kp = gimbal_value()
            .child(Step::Member("yaw".into()))
            .child(Step::Member("kp".into()));
        let spelled_out = NodeRef::root(GIMBAL)
            .child(Step::Member("__0".into()))
            .child(Step::Member("value".into()))
            .child(Step::Member("yaw".into()))
            .child(Step::Member("kp".into()));
        let short = node(&elf, &kp).unwrap();
        let long = node(&elf, &spelled_out).unwrap();
        assert_eq!((long.address, long.scalar), (short.address, short.scalar));

        // Stopping on a wrapper member gives the value inside it
        let cell = node(
            &elf,
            &NodeRef::root(GIMBAL).child(Step::Member("__0".into())),
        )
        .unwrap();
        assert_eq!(cell.type_name, "Gimbal<4>");
    }

    #[test]
    fn runtime_statics_are_internal() {
        assert!(is_internal(
            "sentry::led_task::POOL",
            ".bss",
            "TaskPoolHolder<104, 8>"
        ));
        assert!(is_internal(
            "app::CAN3_BUFFERS",
            ".bss",
            "StaticCell<bsp::FdCanBuffers>"
        ));
        assert!(is_internal(
            "_SEGGER_RTT",
            ".rtt",
            "MaybeUninit<RttControlBlock>"
        ));
        assert!(is_internal(
            "hal::rtt::init::_RTT_CHANNEL_BUFFER",
            ".rtt",
            "MaybeUninit<[u8; 512]>"
        ));
        assert!(is_internal("embassy_stm32::dma::STATE", ".bss", "State"));
        assert!(is_internal(
            "<embassy_stm32::_generated::peripherals::UART5 as embassy_stm32::usart::SealedInstance>::state::STATE",
            ".bss",
            "State"
        ));
        assert!(!is_internal(
            "app::transport::DR16_ERRORS",
            ".bss",
            "Atomic<u32>"
        ));
        assert!(!is_internal(
            "app::TELEMETRY",
            ".bss",
            "Signal<CriticalSectionRawMutex, Telemetry>"
        ));

        let elf = rust();
        assert!(roots(&elf).iter().all(|r| !r.internal));
    }

    #[test]
    fn c_structs_browse_the_same_way() {
        let elf = ElfParser::parse_bytes(TEST_STRUCT_ELF, "test_struct.elf").unwrap();
        let cfg = NodeRef::root("device_config");
        let kids = children(&elf, &cfg, None).unwrap();
        assert_eq!(labels(&kids), ["id", "sensor", "enabled"]);
        let value = node(
            &elf,
            &cfg.child(Step::Member("sensor".into()))
                .child(Step::Member("value".into())),
        )
        .unwrap();
        assert_eq!(value.scalar, Some(VariableType::F32));
        assert_eq!(
            value.address,
            elf.find_symbol("device_config").unwrap().address + 12
        );
    }

    #[test]
    fn node_ref_serializes_as_tagged_steps() {
        let r = NodeRef::root("X")
            .child(Step::Member("a".into()))
            .child(Step::Index(2))
            .child(Step::Discriminant);
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(
            json,
            r#"{"symbol":"X","steps":[{"kind":"member","value":"a"},{"kind":"index","value":2},{"kind":"discriminant"}]}"#
        );
        assert_eq!(serde_json::from_str::<NodeRef>(&json).unwrap(), r);
    }
}
