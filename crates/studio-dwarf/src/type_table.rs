//! Global Type Table for DWARF Type Resolution
//!
//! This module provides a centralized type table where types are stored by index (TypeId).
//! All type references use TypeId instead of `Box<TypeInfo>`, which simplifies:
//! - Forward declaration resolution (just update the target)
//! - Template support (template instantiations share base type references)
//! - Memory efficiency (no deep nesting of boxed types)
//!
//! The TypeHandle struct wraps `Arc<TypeTable>` + TypeId for zero-overhead type access.

use crate::variable_type::VariableType;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A unique identifier for a type in the global type table.
/// This is just an index into the types vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TypeId(pub u32);

impl TypeId {
    /// The invalid/unresolved type ID
    pub const INVALID: TypeId = TypeId(u32::MAX);

    /// Check if this is a valid type ID
    pub fn is_valid(self) -> bool {
        self != Self::INVALID
    }
}

impl std::fmt::Display for TypeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if *self == Self::INVALID {
            write!(f, "TypeId(INVALID)")
        } else {
            write!(f, "TypeId({})", self.0)
        }
    }
}

/// Key for mapping DWARF DIE locations to TypeIds
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DwarfTypeKey {
    pub unit_index: usize,
    pub offset: usize,
}

impl DwarfTypeKey {
    pub fn new(unit_index: usize, offset: usize) -> Self {
        Self { unit_index, offset }
    }
}

/// A unified key that can represent either a unit-local or global DWARF type reference.
/// ARM compilers often use DW_FORM_ref_addr (global offsets) instead of DW_FORM_ref* (unit-local).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlobalTypeKey {
    /// Unit-local reference (unit_index + local offset within the unit)
    UnitLocal(DwarfTypeKey),
    /// Global reference (absolute offset in .debug_info section)
    /// We use usize::MAX as a sentinel unit_index for global refs
    Global(usize),
}

impl GlobalTypeKey {
    /// Create a unit-local key
    pub fn unit_local(unit_index: usize, offset: usize) -> Self {
        GlobalTypeKey::UnitLocal(DwarfTypeKey::new(unit_index, offset))
    }

    /// Create a global key from an absolute .debug_info offset
    pub fn global(offset: usize) -> Self {
        GlobalTypeKey::Global(offset)
    }

    /// Convert to DwarfTypeKey for backwards compatibility
    /// Global keys use a sentinel unit_index of usize::MAX
    pub fn to_dwarf_key(&self) -> DwarfTypeKey {
        match self {
            GlobalTypeKey::UnitLocal(key) => *key,
            GlobalTypeKey::Global(offset) => DwarfTypeKey::new(usize::MAX, *offset),
        }
    }
}

impl From<DwarfTypeKey> for GlobalTypeKey {
    fn from(key: DwarfTypeKey) -> Self {
        GlobalTypeKey::UnitLocal(key)
    }
}

/// A type definition in the type table
#[derive(Debug, Clone)]
pub enum TypeDef {
    /// Primitive/base types (int, float, bool, etc.)
    Primitive(PrimitiveDef),
    /// Pointer to another type
    Pointer(TypeId),
    /// Reference to another type (C++)
    Reference(TypeId),
    /// Array of elements
    Array { element: TypeId, count: Option<u64> },
    /// Struct or class
    Struct(StructDef),
    /// Union type
    Union(StructDef),
    /// Enumeration
    Enum(EnumDef),
    /// Typedef/alias
    Typedef { name: String, underlying: TypeId },
    /// Const qualifier
    Const(TypeId),
    /// Volatile qualifier
    Volatile(TypeId),
    /// Restrict qualifier
    Restrict(TypeId),
    /// Subroutine/function type
    Subroutine {
        return_type: Option<TypeId>,
        params: Vec<TypeId>,
    },
    /// Void type
    Void,
    /// Unknown type with size
    Unknown { size: u64 },
    /// Forward declaration - will be resolved to point to actual definition
    ForwardDecl {
        name: String,
        kind: ForwardDeclKind,
        /// Once resolved, this points to the actual type
        target: Option<TypeId>,
    },
    /// Placeholder for types being parsed (used during construction)
    Placeholder,
}

impl TypeDef {
    /// Check if this is a forward declaration that hasn't been resolved
    pub fn is_unresolved_forward_decl(&self) -> bool {
        matches!(self, TypeDef::ForwardDecl { target: None, .. })
    }

    /// Check if this is a forward declaration (resolved or not)
    pub fn is_forward_decl(&self) -> bool {
        matches!(self, TypeDef::ForwardDecl { .. })
    }

    /// Get the name of this type if it has one
    pub fn name(&self) -> Option<&str> {
        match self {
            TypeDef::Struct(s) | TypeDef::Union(s) => s.name.as_deref(),
            TypeDef::Enum(e) => e.name.as_deref(),
            TypeDef::Typedef { name, .. } => Some(name),
            TypeDef::ForwardDecl { name, .. } => Some(name),
            TypeDef::Primitive(p) => Some(p.name()),
            _ => None,
        }
    }

    /// Check if this type can be expanded (has members)
    pub fn is_expandable(&self) -> bool {
        match self {
            TypeDef::Struct(s) | TypeDef::Union(s) => !s.members.is_empty(),
            TypeDef::ForwardDecl {
                target: Some(_), ..
            } => true, // Will check target
            _ => false,
        }
    }
}

/// The kind of forward declaration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardDeclKind {
    Struct,
    Class,
    Union,
    Enum,
}

/// Primitive type definition
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveDef {
    Bool,
    Char,
    /// Unicode code unit; unlike C char, its DWARF byte size is significant.
    UnicodeChar {
        size: u64,
    },
    SignedChar,
    UnsignedChar,
    Short,
    UnsignedShort,
    Int,
    UnsignedInt,
    Long,
    UnsignedLong,
    LongLong,
    UnsignedLongLong,
    Float,
    Double,
    LongDouble,
    /// Sized integer (for unusual sizes)
    SizedInt {
        size: u64,
        signed: bool,
    },
    /// Sized float (for unusual sizes)
    SizedFloat {
        size: u64,
    },
}

impl PrimitiveDef {
    /// Get the size in bytes
    pub fn size(&self) -> u64 {
        match self {
            PrimitiveDef::Bool
            | PrimitiveDef::Char
            | PrimitiveDef::SignedChar
            | PrimitiveDef::UnsignedChar => 1,
            PrimitiveDef::Short | PrimitiveDef::UnsignedShort => 2,
            PrimitiveDef::Int | PrimitiveDef::UnsignedInt | PrimitiveDef::Float => 4,
            PrimitiveDef::Long | PrimitiveDef::UnsignedLong => 4,
            PrimitiveDef::LongLong | PrimitiveDef::UnsignedLongLong | PrimitiveDef::Double => 8,
            PrimitiveDef::LongDouble => 16,
            PrimitiveDef::SizedInt { size, .. } => *size,
            PrimitiveDef::SizedFloat { size } | PrimitiveDef::UnicodeChar { size } => *size,
        }
    }

    /// Get the name of this primitive type
    pub fn name(&self) -> &'static str {
        match self {
            PrimitiveDef::Bool => "bool",
            PrimitiveDef::Char | PrimitiveDef::UnicodeChar { .. } => "char",
            PrimitiveDef::SignedChar => "signed char",
            PrimitiveDef::UnsignedChar => "unsigned char",
            PrimitiveDef::Short => "short",
            PrimitiveDef::UnsignedShort => "unsigned short",
            PrimitiveDef::Int => "int",
            PrimitiveDef::UnsignedInt => "unsigned int",
            PrimitiveDef::Long => "long",
            PrimitiveDef::UnsignedLong => "unsigned long",
            PrimitiveDef::LongLong => "long long",
            PrimitiveDef::UnsignedLongLong => "unsigned long long",
            PrimitiveDef::Float => "float",
            PrimitiveDef::Double => "double",
            PrimitiveDef::LongDouble => "long double",
            PrimitiveDef::SizedInt { signed: true, .. } => "int",
            PrimitiveDef::SizedInt { signed: false, .. } => "unsigned int",
            PrimitiveDef::SizedFloat { .. } => "float",
        }
    }

    /// Check if this is a signed type
    pub fn is_signed(&self) -> bool {
        match self {
            PrimitiveDef::SignedChar
            | PrimitiveDef::Short
            | PrimitiveDef::Int
            | PrimitiveDef::Long
            | PrimitiveDef::LongLong => true,
            PrimitiveDef::SizedInt { signed, .. } => *signed,
            PrimitiveDef::Float
            | PrimitiveDef::Double
            | PrimitiveDef::LongDouble
            | PrimitiveDef::SizedFloat { .. } => true,
            _ => false,
        }
    }

    /// Check if this is a floating point type
    pub fn is_float(&self) -> bool {
        matches!(
            self,
            PrimitiveDef::Float
                | PrimitiveDef::Double
                | PrimitiveDef::LongDouble
                | PrimitiveDef::SizedFloat { .. }
        )
    }

    /// Convert to VariableType
    pub fn to_variable_type(&self) -> VariableType {
        match self {
            PrimitiveDef::UnicodeChar { size } => match size {
                1 => VariableType::U8,
                2 => VariableType::U16,
                4 => VariableType::U32,
                _ => VariableType::Raw(*size as usize),
            },
            PrimitiveDef::Bool => VariableType::Bool,
            PrimitiveDef::Char | PrimitiveDef::SignedChar => VariableType::I8,
            PrimitiveDef::UnsignedChar => VariableType::U8,
            PrimitiveDef::Short => VariableType::I16,
            PrimitiveDef::UnsignedShort => VariableType::U16,
            PrimitiveDef::Int | PrimitiveDef::Long => VariableType::I32,
            PrimitiveDef::UnsignedInt | PrimitiveDef::UnsignedLong => VariableType::U32,
            PrimitiveDef::LongLong => VariableType::I64,
            PrimitiveDef::UnsignedLongLong => VariableType::U64,
            PrimitiveDef::Float => VariableType::F32,
            PrimitiveDef::Double | PrimitiveDef::LongDouble => VariableType::F64,
            PrimitiveDef::SizedInt { size, signed } => match (size, signed) {
                (1, true) => VariableType::I8,
                (1, false) => VariableType::U8,
                (2, true) => VariableType::I16,
                (2, false) => VariableType::U16,
                (4, true) => VariableType::I32,
                (4, false) => VariableType::U32,
                (8, true) => VariableType::I64,
                (8, false) => VariableType::U64,
                _ => VariableType::Raw(*size as usize),
            },
            PrimitiveDef::SizedFloat { size } => match size {
                4 => VariableType::F32,
                8 => VariableType::F64,
                _ => VariableType::Raw(*size as usize),
            },
        }
    }
}

/// Struct/class/union definition
#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: Option<String>,
    pub size: u64,
    pub members: Vec<MemberDef>,
    pub is_class: bool,
    /// Template parameters (empty for non-template types)
    pub template_params: Vec<TemplateParam>,
    /// Base classes for C++ inheritance
    pub base_classes: Vec<BaseClassDef>,
    /// Rust enum layout (`DW_TAG_variant_part`); `members` is empty when set
    pub variant_part: Option<VariantPart>,
}

impl StructDef {
    pub fn new(name: Option<String>, size: u64, is_class: bool) -> Self {
        Self {
            name,
            size,
            members: Vec::new(),
            is_class,
            template_params: Vec::new(),
            base_classes: Vec::new(),
            variant_part: None,
        }
    }

    /// Get the name as the compiler wrote it
    pub fn display_name_without_params(&self) -> String {
        self.name.as_deref().unwrap_or("<anonymous>").to_string()
    }

    /// Get the display name including template parameters
    pub fn display_name(&self) -> String {
        let base_name = self.name.as_deref().unwrap_or("<anonymous>");
        // rustc and GCC already spell the arguments into the name (`Atomic<u32>`)
        if self.template_params.is_empty() || base_name.contains('<') {
            base_name.to_string()
        } else {
            let params: Vec<String> = self.template_params.iter().map(|p| p.display()).collect();
            format!("{}<{}>", base_name, params.join(", "))
        }
    }
}

/// A member of a struct/union
#[derive(Debug, Clone)]
pub struct MemberDef {
    pub name: String,
    pub offset: u64,
    pub type_id: TypeId,
    pub bit_offset: Option<u64>,
    pub bit_size: Option<u64>,
}

impl MemberDef {
    pub fn new(name: String, offset: u64, type_id: TypeId) -> Self {
        Self {
            name,
            offset,
            type_id,
            bit_offset: None,
            bit_size: None,
        }
    }

    /// Check if this is a bit field
    pub fn is_bitfield(&self) -> bool {
        self.bit_size.is_some()
    }
}

/// Layout of a Rust enum with data, from `DW_TAG_variant_part`.
///
/// Read the `discriminant` member's raw bits and pass them to [`VariantPart::select`]
/// to find the active variant. For niche-encoded enums the discriminant lives inside
/// another variant's payload and the untagged (dataful) variant has no `discr_value`.
#[derive(Debug, Clone, Default)]
pub struct VariantPart {
    /// The tag member (offset/type within the enum). `None` for single-variant enums.
    pub discriminant: Option<MemberDef>,
    pub variants: Vec<VariantDef>,
}

impl VariantPart {
    /// Active variant for a raw discriminant value: exact match, else the default variant.
    pub fn select(&self, discr: u64) -> Option<&VariantDef> {
        self.variants
            .iter()
            .find(|v| v.discr_value == Some(discr))
            .or_else(|| self.variants.iter().find(|v| v.discr_value.is_none()))
    }
}

/// One enum variant; `member` is named after the variant and typed as its payload struct.
#[derive(Debug, Clone)]
pub struct VariantDef {
    pub discr_value: Option<u64>,
    pub member: MemberDef,
    /// Where the variant is declared. For an `async fn`'s state, a suspend
    /// variant points at its `.await`.
    pub location: Option<SourceLocation>,
}

/// A file and line from `DW_AT_decl_file` / `DW_AT_decl_line`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceLocation {
    /// As the compiler recorded it; absolute for Rust builds
    pub file: String,
    pub line: u64,
}

/// Base class for C++ inheritance
#[derive(Debug, Clone)]
pub struct BaseClassDef {
    pub type_id: TypeId,
    pub offset: u64,
    pub is_virtual: bool,
}

/// Template parameter
#[derive(Debug, Clone)]
pub enum TemplateParam {
    /// Type parameter (e.g., T in `vector<T>`)
    Type { name: String, type_id: TypeId },
    /// Value parameter (e.g., N in `array<T, N>`)
    Value { name: String, value: i64 },
}

impl TemplateParam {
    pub fn display(&self) -> String {
        match self {
            TemplateParam::Type { name, .. } => name.clone(),
            TemplateParam::Value { value, .. } => value.to_string(),
        }
    }
}

/// Enumeration definition
#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: Option<String>,
    pub size: u64,
    pub variants: Vec<EnumVariant>,
    pub is_scoped: bool,
    pub underlying_type: Option<TypeId>,
}

impl EnumDef {
    pub fn new(name: Option<String>, size: u64, is_scoped: bool) -> Self {
        Self {
            name,
            size,
            variants: Vec::new(),
            is_scoped,
            underlying_type: None,
        }
    }

    /// Find a variant by value
    pub fn find_variant(&self, value: i64) -> Option<&EnumVariant> {
        self.variants.iter().find(|v| v.value == value)
    }

    /// Convert a value to its variant name
    pub fn value_to_string(&self, value: i64) -> String {
        if let Some(variant) = self.find_variant(value) {
            if self.is_scoped {
                if let Some(name) = &self.name {
                    return format!("{}::{}", name, variant.name);
                }
            }
            variant.name.clone()
        } else {
            value.to_string()
        }
    }
}

/// An enum variant/enumerator
#[derive(Debug, Clone)]
pub struct EnumVariant {
    pub name: String,
    pub value: i64,
}

/// The global type table
#[derive(Debug)]
pub struct TypeTable {
    /// All type definitions, indexed by TypeId
    types: Vec<TypeDef>,
    /// Maps DWARF DIE location to TypeId for deduplication
    dwarf_to_id: HashMap<DwarfTypeKey, TypeId>,
    /// Maps type names to their canonical TypeId (for forward declaration resolution)
    name_to_id: HashMap<String, Vec<TypeId>>,
    /// Pending forward declarations that need resolution
    pending_forward_decls: Vec<TypeId>,
    /// Types declared in Rust compile units; named with Rust syntax
    rust_types: HashSet<TypeId>,
    /// Compiler-provided names that the structural name cannot reproduce
    /// (Rust `u16`, `&mut T`, `fn() -> !`)
    source_names: HashMap<TypeId, String>,
}

impl Default for TypeTable {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeTable {
    /// Create a new empty type table
    pub fn new() -> Self {
        Self {
            types: Vec::new(),
            dwarf_to_id: HashMap::new(),
            name_to_id: HashMap::new(),
            pending_forward_decls: Vec::new(),
            rust_types: HashSet::new(),
            source_names: HashMap::new(),
        }
    }

    /// Mark a type as coming from a Rust compile unit
    pub fn mark_rust(&mut self, id: TypeId) {
        self.rust_types.insert(id);
    }

    /// Whether a type was declared in a Rust compile unit
    pub fn is_rust(&self, id: TypeId) -> bool {
        self.rust_types.contains(&id)
    }

    /// Record the compiler's own spelling of a type name
    pub fn set_source_name(&mut self, id: TypeId, name: String) {
        self.source_names.insert(id, name);
    }

    /// Get the number of types in the table
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// Check if the table is empty
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    /// Get the number of DWARF key mappings (including global offset aliases)
    pub fn dwarf_key_count(&self) -> usize {
        self.dwarf_to_id.len()
    }

    /// Allocate a new type ID without setting its definition
    pub fn allocate(&mut self) -> TypeId {
        let id = TypeId(self.types.len() as u32);
        self.types.push(TypeDef::Placeholder);
        id
    }

    /// Get or create a TypeId for a DWARF key
    pub fn get_or_allocate(&mut self, key: DwarfTypeKey) -> TypeId {
        if let Some(&id) = self.dwarf_to_id.get(&key) {
            return id;
        }
        let id = self.allocate();
        self.dwarf_to_id.insert(key, id);
        id
    }

    /// Get the TypeId for a DWARF key if it exists
    pub fn get_by_dwarf_key(&self, key: DwarfTypeKey) -> Option<TypeId> {
        self.dwarf_to_id.get(&key).copied()
    }

    /// Register an existing TypeId under an additional key.
    /// This is used to register the same type under both its unit-local key
    /// and its global .debug_info offset, so that DebugInfoRef lookups work.
    pub fn register_alias(&mut self, key: DwarfTypeKey, id: TypeId) {
        self.dwarf_to_id.entry(key).or_insert(id);
    }

    /// Define a type at the given TypeId
    pub fn define(&mut self, id: TypeId, def: TypeDef) {
        assert!(
            (id.0 as usize) < self.types.len(),
            "TypeId {} out of range",
            id.0
        );

        if def.is_forward_decl() {
            self.pending_forward_decls.push(id);
        }

        if let Some(name) = def.name() {
            if !def.is_forward_decl() {
                self.name_to_id
                    .entry(name.to_string())
                    .or_default()
                    .push(id);
            }
        }

        self.types[id.0 as usize] = def;
    }

    /// Insert a new type and return its TypeId
    pub fn insert(&mut self, def: TypeDef) -> TypeId {
        let id = self.allocate();
        self.define(id, def);
        id
    }

    /// Insert a type for a DWARF key
    pub fn insert_for_key(&mut self, key: DwarfTypeKey, def: TypeDef) -> TypeId {
        let id = self.get_or_allocate(key);
        self.define(id, def);
        id
    }

    /// Get a type definition by TypeId
    pub fn get(&self, id: TypeId) -> Option<&TypeDef> {
        if id == TypeId::INVALID {
            return None;
        }
        self.types.get(id.0 as usize)
    }

    /// Get a mutable reference to a type definition
    pub fn get_mut(&mut self, id: TypeId) -> Option<&mut TypeDef> {
        if id == TypeId::INVALID {
            return None;
        }
        self.types.get_mut(id.0 as usize)
    }

    /// Resolve a TypeId, following forward declarations to their targets
    pub fn resolve(&self, id: TypeId) -> TypeId {
        if let Some(TypeDef::ForwardDecl {
            target: Some(target),
            ..
        }) = self.get(id)
        {
            self.resolve(*target)
        } else {
            id
        }
    }

    /// Get the resolved type definition
    pub fn get_resolved(&self, id: TypeId) -> Option<&TypeDef> {
        self.get(self.resolve(id))
    }

    /// Find all TypeIds for types with the given name
    pub fn find_by_name(&self, name: &str) -> Vec<TypeId> {
        self.name_to_id.get(name).cloned().unwrap_or_default()
    }

    /// Find the best definition for a type name
    pub fn find_best_definition(&self, name: &str, kind: ForwardDeclKind) -> Option<TypeId> {
        let candidates = self.find_by_name(name);
        let mut best: Option<(TypeId, usize)> = None;

        for &id in &candidates {
            if let Some(def) = self.get(id) {
                let (matches_kind, member_count) = match (kind, def) {
                    (ForwardDeclKind::Struct | ForwardDeclKind::Class, TypeDef::Struct(s)) => {
                        (true, s.members.len())
                    }
                    (ForwardDeclKind::Union, TypeDef::Union(s)) => (true, s.members.len()),
                    (ForwardDeclKind::Enum, TypeDef::Enum(e)) => (true, e.variants.len()),
                    _ => (false, 0),
                };

                if matches_kind && best.is_none_or(|(_, best_count)| member_count > best_count) {
                    best = Some((id, member_count));
                }
            }
        }

        best.map(|(id, _)| id)
    }

    /// Resolve all pending forward declarations
    pub fn resolve_forward_declarations(&mut self) {
        let pending = std::mem::take(&mut self.pending_forward_decls);
        let mut resolved_count = 0;

        for id in pending {
            if let Some(TypeDef::ForwardDecl { name, kind, target }) = self.get(id).cloned() {
                if target.is_some() {
                    continue;
                }

                if let Some(best_id) = self.find_best_definition(&name, kind) {
                    if best_id != id {
                        if let Some(TypeDef::ForwardDecl { target, .. }) = self.get_mut(id) {
                            *target = Some(best_id);
                            resolved_count += 1;
                        }
                    }
                }
            }
        }

        tracing::debug!("Resolved {} forward declarations", resolved_count);
    }

    /// Get all type IDs in the table
    pub fn all_type_ids(&self) -> impl Iterator<Item = TypeId> {
        (0..self.types.len() as u32).map(TypeId)
    }

    /// Get the size of a type in bytes
    pub fn type_size(&self, id: TypeId) -> Option<u64> {
        let resolved_id = self.resolve(id);
        match self.get(resolved_id)? {
            TypeDef::Primitive(p) => Some(p.size()),
            TypeDef::Pointer(_) | TypeDef::Reference(_) => Some(4),
            TypeDef::Array { element, count } => {
                let elem_size = self.type_size(*element)?;
                Some(elem_size * count.unwrap_or(0))
            }
            TypeDef::Struct(s) | TypeDef::Union(s) => Some(s.size),
            TypeDef::Enum(e) => Some(e.size),
            TypeDef::Typedef { underlying, .. } => self.type_size(*underlying),
            TypeDef::Const(inner) | TypeDef::Volatile(inner) | TypeDef::Restrict(inner) => {
                self.type_size(*inner)
            }
            TypeDef::Subroutine { .. } => Some(4),
            TypeDef::Void => Some(0),
            TypeDef::Unknown { size } => Some(*size),
            TypeDef::ForwardDecl {
                target: Some(target),
                ..
            } => self.type_size(*target),
            TypeDef::ForwardDecl { target: None, .. } | TypeDef::Placeholder => None,
        }
    }

    /// Get the display name of a type
    pub fn type_name(&self, id: TypeId) -> String {
        self.type_name_with_depth(id, 0)
    }

    fn type_name_with_depth(&self, id: TypeId, depth: usize) -> String {
        if depth > 20 {
            return "<recursive>".to_string();
        }

        let resolved_id = self.resolve(id);
        if let Some(name) = self.source_names.get(&resolved_id) {
            return name.clone();
        }
        if self.is_rust(resolved_id) {
            if let Some(name) = self.rust_type_name(resolved_id, depth) {
                return name;
            }
        }
        match self.get(resolved_id) {
            None => format!("<invalid:{}>", id.0),
            Some(def) => match def {
                TypeDef::Primitive(p) => p.name().to_string(),
                TypeDef::Pointer(inner) => {
                    format!("{}*", self.type_name_with_depth(*inner, depth + 1))
                }
                TypeDef::Reference(inner) => {
                    format!("{}&", self.type_name_with_depth(*inner, depth + 1))
                }
                TypeDef::Array { element, count } => {
                    let elem_name = self.type_name_with_depth(*element, depth + 1);
                    match count {
                        Some(n) => format!("{}[{}]", elem_name, n),
                        None => format!("{}[]", elem_name),
                    }
                }
                TypeDef::Struct(s) => s.display_name(),
                TypeDef::Union(s) => format!("union {}", s.display_name()),
                TypeDef::Enum(e) => e.name.as_deref().unwrap_or("<anonymous enum>").to_string(),
                TypeDef::Typedef { name, .. } => name.clone(),
                TypeDef::Const(inner) => {
                    format!("const {}", self.type_name_with_depth(*inner, depth + 1))
                }
                TypeDef::Volatile(inner) => {
                    format!("volatile {}", self.type_name_with_depth(*inner, depth + 1))
                }
                TypeDef::Restrict(inner) => {
                    format!("restrict {}", self.type_name_with_depth(*inner, depth + 1))
                }
                TypeDef::Subroutine { return_type, .. } => {
                    let ret = return_type
                        .map(|r| self.type_name_with_depth(r, depth + 1))
                        .unwrap_or_else(|| "void".to_string());
                    format!("{}()", ret)
                }
                TypeDef::Void => "void".to_string(),
                TypeDef::Unknown { size } => format!("unknown[{}]", size),
                TypeDef::ForwardDecl { name, kind, target } => {
                    if let Some(target) = target {
                        self.type_name_with_depth(*target, depth + 1)
                    } else {
                        let prefix = match kind {
                            ForwardDeclKind::Struct => "struct",
                            ForwardDeclKind::Class => "class",
                            ForwardDeclKind::Union => "union",
                            ForwardDeclKind::Enum => "enum",
                        };
                        format!("{} {} (forward)", prefix, name)
                    }
                }
                TypeDef::Placeholder => "<placeholder>".to_string(),
            },
        }
    }

    /// Rust spellings for the structural types that differ from C
    fn rust_type_name(&self, id: TypeId, depth: usize) -> Option<String> {
        Some(match self.get(id)? {
            TypeDef::Array { element, count } => {
                let elem = self.type_name_with_depth(*element, depth + 1);
                match count {
                    Some(n) => format!("[{elem}; {n}]"),
                    None => format!("[{elem}]"),
                }
            }
            TypeDef::Pointer(inner) => {
                format!("*const {}", self.type_name_with_depth(*inner, depth + 1))
            }
            // rustc spells concrete arguments into the name; template params are
            // inherited by variant payload structs and would print `Some<T>`
            TypeDef::Struct(s) | TypeDef::Union(s) => s.display_name_without_params(),
            TypeDef::Void => "()".to_string(),
            _ => return None,
        })
    }

    /// Check if a type is expandable (has members that can be displayed)
    pub fn is_expandable(&self, id: TypeId) -> bool {
        self.is_expandable_with_depth(id, 0)
    }

    fn is_expandable_with_depth(&self, id: TypeId, depth: usize) -> bool {
        if depth > 20 {
            return false;
        }

        let resolved_id = self.resolve(id);
        match self.get(resolved_id) {
            None => false,
            Some(def) => match def {
                TypeDef::Struct(s) | TypeDef::Union(s) => {
                    !s.members.is_empty()
                        || s.variant_part
                            .as_ref()
                            .is_some_and(|v| !v.variants.is_empty())
                }
                // Arrays are expandable if they have a known count > 0
                TypeDef::Array { count, .. } => count.map(|c| c > 0).unwrap_or(false),
                // Pointers/references are expandable if they point to an expandable type
                TypeDef::Pointer(inner) | TypeDef::Reference(inner) => {
                    self.is_expandable_with_depth(*inner, depth + 1)
                }
                TypeDef::Typedef { underlying, .. } => {
                    self.is_expandable_with_depth(*underlying, depth + 1)
                }
                TypeDef::Const(inner) | TypeDef::Volatile(inner) | TypeDef::Restrict(inner) => {
                    self.is_expandable_with_depth(*inner, depth + 1)
                }
                TypeDef::ForwardDecl {
                    target: Some(target),
                    ..
                } => self.is_expandable_with_depth(*target, depth + 1),
                _ => false,
            },
        }
    }

    /// Check if a type can be added as a watchable variable
    pub fn is_addable(&self, id: TypeId) -> bool {
        let resolved_id = self.resolve(id);
        match self.get(resolved_id) {
            Some(TypeDef::Primitive(_)) => true,
            Some(TypeDef::Enum(_)) => true,
            Some(TypeDef::Pointer(_)) | Some(TypeDef::Reference(_)) => true,
            Some(TypeDef::Typedef { underlying, .. }) => self.is_addable(*underlying),
            Some(TypeDef::Const(inner))
            | Some(TypeDef::Volatile(inner))
            | Some(TypeDef::Restrict(inner)) => self.is_addable(*inner),
            Some(TypeDef::Struct(_)) | Some(TypeDef::Union(_)) | Some(TypeDef::Array { .. }) => {
                true
            }
            Some(TypeDef::Unknown { size }) => *size > 0,
            _ => false,
        }
    }

    /// Get the members of a type (if it's a struct/union)
    pub fn get_members(&self, id: TypeId) -> Option<&[MemberDef]> {
        self.get_members_with_depth(id, 0)
    }

    fn get_members_with_depth(&self, id: TypeId, depth: usize) -> Option<&[MemberDef]> {
        if depth > 20 {
            return None;
        }

        let resolved_id = self.resolve(id);
        match self.get(resolved_id)? {
            TypeDef::Struct(s) | TypeDef::Union(s) => Some(&s.members),
            TypeDef::Typedef { underlying, .. } => {
                self.get_members_with_depth(*underlying, depth + 1)
            }
            TypeDef::Const(inner) | TypeDef::Volatile(inner) | TypeDef::Restrict(inner) => {
                self.get_members_with_depth(*inner, depth + 1)
            }
            TypeDef::ForwardDecl {
                target: Some(target),
                ..
            } => self.get_members_with_depth(*target, depth + 1),
            _ => None,
        }
    }

    /// Get the underlying type for typedefs, const, volatile, etc.
    pub fn get_underlying(&self, id: TypeId) -> TypeId {
        self.get_underlying_with_depth(id, 0)
    }

    fn get_underlying_with_depth(&self, id: TypeId, depth: usize) -> TypeId {
        if depth > 20 {
            return id;
        }

        let resolved_id = self.resolve(id);
        match self.get(resolved_id) {
            Some(TypeDef::Typedef { underlying, .. })
            | Some(TypeDef::Const(underlying))
            | Some(TypeDef::Volatile(underlying))
            | Some(TypeDef::Restrict(underlying)) => {
                self.get_underlying_with_depth(*underlying, depth + 1)
            }
            _ => resolved_id,
        }
    }

    /// Convert a TypeId to VariableType
    pub fn to_variable_type(&self, id: TypeId) -> VariableType {
        let resolved_id = self.resolve(id);
        match self.get(resolved_id) {
            Some(TypeDef::Primitive(prim)) => prim.to_variable_type(),
            Some(TypeDef::Pointer(_)) | Some(TypeDef::Reference(_)) => VariableType::U32,
            Some(TypeDef::Enum(e)) => match e.size {
                1 => VariableType::U8,
                2 => VariableType::U16,
                4 => VariableType::U32,
                8 => VariableType::U64,
                _ => VariableType::Raw(e.size as usize),
            },
            Some(TypeDef::Struct(s)) | Some(TypeDef::Union(s)) => {
                VariableType::Raw(s.size as usize)
            }
            Some(TypeDef::Array { element, count }) => {
                let elem_size = self.type_size(*element).unwrap_or(0);
                let total = elem_size * count.unwrap_or(1);
                VariableType::Raw(total as usize)
            }
            Some(TypeDef::Typedef { underlying, .. }) => self.to_variable_type(*underlying),
            Some(TypeDef::Const(inner))
            | Some(TypeDef::Volatile(inner))
            | Some(TypeDef::Restrict(inner)) => self.to_variable_type(*inner),
            Some(TypeDef::Subroutine { .. }) => VariableType::U32,
            Some(TypeDef::Unknown { size }) => VariableType::Raw(*size as usize),
            _ => VariableType::Raw(0),
        }
    }

    /// Iterate over all types
    pub fn iter(&self) -> impl Iterator<Item = (TypeId, &TypeDef)> {
        self.types
            .iter()
            .enumerate()
            .map(|(i, def)| (TypeId(i as u32), def))
    }

    /// Get statistics about the type table
    pub fn stats(&self) -> TypeTableStats {
        let mut stats = TypeTableStats {
            total_types: self.types.len(),
            ..Default::default()
        };

        for def in &self.types {
            match def {
                TypeDef::Primitive(_) => stats.primitives += 1,
                TypeDef::Pointer(_) => stats.pointers += 1,
                TypeDef::Reference(_) => stats.references += 1,
                TypeDef::Array { .. } => stats.arrays += 1,
                TypeDef::Struct(_) => stats.structs += 1,
                TypeDef::Union(_) => stats.unions += 1,
                TypeDef::Enum(_) => stats.enums += 1,
                TypeDef::Typedef { .. } => stats.typedefs += 1,
                TypeDef::Const(_) | TypeDef::Volatile(_) | TypeDef::Restrict(_) => {
                    stats.qualifiers += 1
                }
                TypeDef::Subroutine { .. } => stats.subroutines += 1,
                TypeDef::Void => stats.voids += 1,
                TypeDef::Unknown { .. } => stats.unknowns += 1,
                TypeDef::ForwardDecl { target, .. } => {
                    stats.forward_decls += 1;
                    if target.is_none() {
                        stats.unresolved_forward_decls += 1;
                    }
                }
                TypeDef::Placeholder => stats.placeholders += 1,
            }
        }

        stats
    }
}

/// Statistics about a type table
#[derive(Debug, Default)]
pub struct TypeTableStats {
    pub total_types: usize,
    pub primitives: usize,
    pub pointers: usize,
    pub references: usize,
    pub arrays: usize,
    pub structs: usize,
    pub unions: usize,
    pub enums: usize,
    pub typedefs: usize,
    pub qualifiers: usize,
    pub subroutines: usize,
    pub voids: usize,
    pub unknowns: usize,
    pub forward_decls: usize,
    pub unresolved_forward_decls: usize,
    pub placeholders: usize,
}

// ==================== TypeHandle: Zero-overhead type access ====================

/// A handle to a type that wraps `Arc<TypeTable>` + TypeId.
/// This provides zero-overhead cloning and direct method access.
#[derive(Debug, Clone)]
pub struct TypeHandle {
    table: Arc<TypeTable>,
    id: TypeId,
}

impl TypeHandle {
    /// Create a new TypeHandle
    pub fn new(table: Arc<TypeTable>, id: TypeId) -> Self {
        Self { table, id }
    }

    /// Create an invalid TypeHandle
    pub fn invalid(table: Arc<TypeTable>) -> Self {
        Self {
            table,
            id: TypeId::INVALID,
        }
    }

    /// Get the TypeId
    pub fn id(&self) -> TypeId {
        self.id
    }

    /// Check if this is a valid type
    pub fn is_valid(&self) -> bool {
        self.id.is_valid() && self.table.get(self.id).is_some()
    }

    /// Get the type definition
    pub fn def(&self) -> Option<&TypeDef> {
        self.table.get(self.id)
    }

    /// Get the resolved type definition (follows forward declarations)
    pub fn resolved_def(&self) -> Option<&TypeDef> {
        self.table.get_resolved(self.id)
    }

    /// Get the resolved TypeHandle
    pub fn resolved(&self) -> TypeHandle {
        TypeHandle::new(self.table.clone(), self.table.resolve(self.id))
    }

    /// Get the type name
    pub fn type_name(&self) -> String {
        self.table.type_name(self.id)
    }

    /// Get the type size in bytes
    pub fn size(&self) -> Option<u64> {
        self.table.type_size(self.id)
    }

    /// Check if this type is expandable
    pub fn is_expandable(&self) -> bool {
        self.table.is_expandable(self.id)
    }

    /// Check if this type can be added as a variable
    pub fn is_addable(&self) -> bool {
        self.table.is_addable(self.id)
    }

    /// Get the members of this type (if struct/union)
    pub fn members(&self) -> Option<&[MemberDef]> {
        self.table.get_members(self.id)
    }

    /// Get the underlying type (strips typedefs, qualifiers)
    pub fn underlying(&self) -> TypeHandle {
        TypeHandle::new(self.table.clone(), self.table.get_underlying(self.id))
    }

    /// Convert to VariableType
    pub fn to_variable_type(&self) -> VariableType {
        self.table.to_variable_type(self.id)
    }

    /// Get a handle to a member's type
    pub fn member_type(&self, member: &MemberDef) -> TypeHandle {
        TypeHandle::new(self.table.clone(), member.type_id)
    }

    /// Get a reference to the underlying type table
    pub fn table(&self) -> &Arc<TypeTable> {
        &self.table
    }

    /// Definition after stripping typedefs, qualifiers and forward declarations.
    /// Kind checks use this so `volatile Foo` and `typedef Foo *Bar` classify
    /// the same as `Foo` and `Foo *`.
    fn underlying_def(&self) -> Option<&TypeDef> {
        self.table.get(self.table.get_underlying(self.id))
    }

    /// Check if this is a struct or union
    pub fn is_struct_or_union(&self) -> bool {
        matches!(
            self.underlying_def(),
            Some(TypeDef::Struct(_)) | Some(TypeDef::Union(_))
        )
    }

    /// Check if this is a pointer or reference
    pub fn is_pointer_or_reference(&self) -> bool {
        matches!(
            self.underlying_def(),
            Some(TypeDef::Pointer(_)) | Some(TypeDef::Reference(_))
        )
    }

    /// Check if this is a primitive type
    pub fn is_primitive(&self) -> bool {
        matches!(self.underlying_def(), Some(TypeDef::Primitive(_)))
    }

    /// Get the pointee type if this is a pointer/reference
    pub fn pointee(&self) -> Option<TypeHandle> {
        match self.underlying_def() {
            Some(TypeDef::Pointer(inner)) | Some(TypeDef::Reference(inner)) => {
                Some(TypeHandle::new(self.table.clone(), *inner))
            }
            _ => None,
        }
    }

    /// Get the array element type if this is an array
    pub fn element_type(&self) -> Option<TypeHandle> {
        match self.underlying_def() {
            Some(TypeDef::Array { element, .. }) => {
                Some(TypeHandle::new(self.table.clone(), *element))
            }
            _ => None,
        }
    }

    /// Get the array count if this is an array
    pub fn array_count(&self) -> Option<u64> {
        match self.underlying_def() {
            Some(TypeDef::Array { count, .. }) => *count,
            _ => None,
        }
    }

    /// Check if this type is an array
    pub fn is_array(&self) -> bool {
        matches!(self.underlying_def(), Some(TypeDef::Array { .. }))
    }

    /// Get the element size if this is an array
    pub fn element_size(&self) -> Option<u64> {
        match self.underlying_def() {
            Some(TypeDef::Array { element, .. }) => self.table.type_size(*element),
            _ => None,
        }
    }

    /// Check if this is a pointer type that can be expanded to show its pointee type
    /// Returns true if this is a pointer/reference to a struct, union, or array
    pub fn is_pointer_expandable(&self) -> bool {
        if let Some(pointee) = self.pointee() {
            let pointee_underlying = pointee.underlying();
            pointee_underlying.is_struct_or_union() || pointee_underlying.is_array()
        } else {
            false
        }
    }

    /// Get the pointee's underlying type handle for pointer expansion
    /// This allows accessing the struct/union members that the pointer points to
    pub fn pointee_underlying(&self) -> Option<TypeHandle> {
        self.pointee().map(|p| p.underlying())
    }
}

/// Shared type table wrapped in Arc for zero-cost cloning
pub type SharedTypeTable = Arc<TypeTable>;

/// Create a shared type table from a TypeTable
pub fn share_type_table(table: TypeTable) -> SharedTypeTable {
    Arc::new(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_id() {
        let id = TypeId(42);
        assert!(id.is_valid());
        assert!(!TypeId::INVALID.is_valid());
        assert_eq!(format!("{}", id), "TypeId(42)");
        assert_eq!(format!("{}", TypeId::INVALID), "TypeId(INVALID)");
    }

    #[test]
    fn test_primitive_def() {
        assert_eq!(PrimitiveDef::Int.size(), 4);
        assert_eq!(PrimitiveDef::LongLong.size(), 8);
        assert!(PrimitiveDef::Int.is_signed());
        assert!(!PrimitiveDef::UnsignedInt.is_signed());
        assert!(PrimitiveDef::Float.is_float());
        assert!(!PrimitiveDef::Int.is_float());
    }

    #[test]
    fn test_type_table_basics() {
        let mut table = TypeTable::new();
        assert!(table.is_empty());

        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));
        assert_eq!(table.len(), 1);
        assert!(!table.is_empty());

        let def = table.get(int_id).unwrap();
        assert!(matches!(def, TypeDef::Primitive(PrimitiveDef::Int)));
        assert_eq!(table.type_size(int_id), Some(4));
        assert_eq!(table.type_name(int_id), "int");
    }

    #[test]
    fn test_dwarf_key_mapping() {
        let mut table = TypeTable::new();
        let key = DwarfTypeKey::new(0, 100);

        let id1 = table.get_or_allocate(key);
        let id2 = table.get_or_allocate(key);
        assert_eq!(id1, id2);

        table.define(id1, TypeDef::Primitive(PrimitiveDef::Float));
        assert_eq!(table.type_name(id1), "float");
    }

    #[test]
    fn test_pointer_type() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));
        let ptr_id = table.insert(TypeDef::Pointer(int_id));

        assert_eq!(table.type_name(ptr_id), "int*");
        assert_eq!(table.type_size(ptr_id), Some(4));
    }

    #[test]
    fn test_struct_type() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));

        let mut struct_def = StructDef::new(Some("Point".to_string()), 8, false);
        struct_def
            .members
            .push(MemberDef::new("x".to_string(), 0, int_id));
        struct_def
            .members
            .push(MemberDef::new("y".to_string(), 4, int_id));

        let struct_id = table.insert(TypeDef::Struct(struct_def));

        assert_eq!(table.type_name(struct_id), "Point");
        assert!(table.is_expandable(struct_id));

        let members = table.get_members(struct_id).unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].name, "x");
        assert_eq!(members[1].name, "y");
    }

    #[test]
    fn test_forward_declaration_resolution() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));

        let fwd_id = table.insert(TypeDef::ForwardDecl {
            name: "Point".to_string(),
            kind: ForwardDeclKind::Struct,
            target: None,
        });

        let mut struct_def = StructDef::new(Some("Point".to_string()), 8, false);
        struct_def
            .members
            .push(MemberDef::new("x".to_string(), 0, int_id));
        let def_id = table.insert(TypeDef::Struct(struct_def));

        assert!(table.get(fwd_id).unwrap().is_unresolved_forward_decl());

        table.resolve_forward_declarations();

        let resolved = table.resolve(fwd_id);
        assert_eq!(resolved, def_id);
        assert!(table.is_expandable(fwd_id));
    }

    #[test]
    fn test_typedef() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));
        let typedef_id = table.insert(TypeDef::Typedef {
            name: "int32_t".to_string(),
            underlying: int_id,
        });

        assert_eq!(table.type_name(typedef_id), "int32_t");
        assert_eq!(table.get_underlying(typedef_id), int_id);
        assert_eq!(table.type_size(typedef_id), Some(4));
    }

    #[test]
    fn test_array_type() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));
        let array_id = table.insert(TypeDef::Array {
            element: int_id,
            count: Some(10),
        });

        assert_eq!(table.type_name(array_id), "int[10]");
        assert_eq!(table.type_size(array_id), Some(40));
    }

    #[test]
    fn test_template_struct() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));

        let mut struct_def = StructDef::new(Some("vector".to_string()), 12, true);
        struct_def.template_params.push(TemplateParam::Type {
            name: "int".to_string(),
            type_id: int_id,
        });
        struct_def
            .members
            .push(MemberDef::new("data".to_string(), 0, int_id));

        let struct_id = table.insert(TypeDef::Struct(struct_def));

        assert_eq!(table.type_name(struct_id), "vector<int>");
    }

    #[test]
    fn test_type_handle() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));

        let mut struct_def = StructDef::new(Some("Point".to_string()), 8, false);
        struct_def
            .members
            .push(MemberDef::new("x".to_string(), 0, int_id));
        struct_def
            .members
            .push(MemberDef::new("y".to_string(), 4, int_id));
        let struct_id = table.insert(TypeDef::Struct(struct_def));

        let shared = Arc::new(table);
        let handle = TypeHandle::new(shared.clone(), struct_id);

        assert!(handle.is_valid());
        assert_eq!(handle.type_name(), "Point");
        assert!(handle.is_expandable());
        assert!(handle.is_struct_or_union());

        let members = handle.members().unwrap();
        assert_eq!(members.len(), 2);

        let x_type = handle.member_type(&members[0]);
        assert!(x_type.is_primitive());
        assert_eq!(x_type.type_name(), "int");
    }

    #[test]
    fn test_type_handle_pointer() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));
        let ptr_id = table.insert(TypeDef::Pointer(int_id));

        let shared = Arc::new(table);
        let handle = TypeHandle::new(shared, ptr_id);

        assert!(handle.is_pointer_or_reference());
        let pointee = handle.pointee().unwrap();
        assert!(pointee.is_primitive());
        assert_eq!(pointee.type_name(), "int");
    }

    #[test]
    fn test_stats() {
        let mut table = TypeTable::new();
        table.insert(TypeDef::Primitive(PrimitiveDef::Int));
        table.insert(TypeDef::Primitive(PrimitiveDef::Float));
        table.insert(TypeDef::Void);
        table.insert(TypeDef::ForwardDecl {
            name: "Foo".to_string(),
            kind: ForwardDeclKind::Struct,
            target: None,
        });

        let stats = table.stats();
        assert_eq!(stats.total_types, 4);
        assert_eq!(stats.primitives, 2);
        assert_eq!(stats.voids, 1);
        assert_eq!(stats.forward_decls, 1);
        assert_eq!(stats.unresolved_forward_decls, 1);
    }

    #[test]
    fn test_typedef_chain_resolution() {
        let mut table = TypeTable::new();

        // Create a chain: typedef -> typedef -> primitive
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));
        let int32_id = table.insert(TypeDef::Typedef {
            name: "int32_t".to_string(),
            underlying: int_id,
        });
        let myint_id = table.insert(TypeDef::Typedef {
            name: "MyInt".to_string(),
            underlying: int32_id,
        });

        // Should resolve through the chain
        let final_type = table.get_underlying(myint_id);
        assert_eq!(final_type, int_id);
        assert_eq!(table.type_size(myint_id), Some(4));
    }

    #[test]
    fn test_recursive_struct_detection() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));

        // Create a struct with a pointer to itself (linked list node)
        let node_fwd = table.insert(TypeDef::ForwardDecl {
            name: "Node".to_string(),
            kind: ForwardDeclKind::Struct,
            target: None,
        });

        let node_ptr_id = table.insert(TypeDef::Pointer(node_fwd));

        let mut node_def = StructDef::new(Some("Node".to_string()), 12, false);
        node_def
            .members
            .push(MemberDef::new("data".to_string(), 0, int_id));
        node_def
            .members
            .push(MemberDef::new("next".to_string(), 4, node_ptr_id));
        let node_id = table.insert(TypeDef::Struct(node_def));

        // Resolve forward declarations
        table.resolve_forward_declarations();

        // Should be able to navigate the structure without infinite loops
        assert_eq!(table.type_name(node_id), "Node");
        assert!(table.is_expandable(node_id));

        let members = table.get_members(node_id).unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[1].name, "next");
    }

    #[test]
    fn test_template_parameter_handling() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));
        let float_id = table.insert(TypeDef::Primitive(PrimitiveDef::Float));

        // Create a templated struct with both type and value parameters
        let mut struct_def = StructDef::new(Some("Container".to_string()), 16, true);
        struct_def.template_params.push(TemplateParam::Type {
            name: "int".to_string(),
            type_id: int_id,
        });
        struct_def.template_params.push(TemplateParam::Value {
            name: "10".to_string(),
            value: 10,
        });
        struct_def
            .members
            .push(MemberDef::new("data".to_string(), 0, float_id));

        let struct_id = table.insert(TypeDef::Struct(struct_def));

        // Check name includes all template parameters
        let name = table.type_name(struct_id);
        assert!(name.contains("Container"));
        assert!(name.contains("int"));
        assert!(name.contains("10"));
    }

    #[test]
    fn test_enum_type() {
        let mut table = TypeTable::new();

        let mut enum_def = EnumDef::new(Some("Color".to_string()), 4, false);
        enum_def.variants.push(EnumVariant {
            name: "Red".to_string(),
            value: 0,
        });
        enum_def.variants.push(EnumVariant {
            name: "Green".to_string(),
            value: 1,
        });
        enum_def.variants.push(EnumVariant {
            name: "Blue".to_string(),
            value: 2,
        });

        let enum_id = table.insert(TypeDef::Enum(enum_def));

        assert_eq!(table.type_name(enum_id), "Color");
        assert_eq!(table.type_size(enum_id), Some(4));
        // Enums may not be marked as expandable in the same way as structs
        // Just check the type exists
        assert!(table.get(enum_id).is_some());
    }

    #[test]
    fn test_union_type() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));
        let float_id = table.insert(TypeDef::Primitive(PrimitiveDef::Float));

        let mut union_def = StructDef::new(Some("Value".to_string()), 4, false);
        union_def
            .members
            .push(MemberDef::new("as_int".to_string(), 0, int_id));
        union_def
            .members
            .push(MemberDef::new("as_float".to_string(), 0, float_id));

        let union_id = table.insert(TypeDef::Union(union_def));

        // Union name includes "union" prefix
        assert!(table.type_name(union_id).contains("Value"));
        assert_eq!(table.type_size(union_id), Some(4));
        assert!(table.is_expandable(union_id));

        let members = table.get_members(union_id).unwrap();
        assert_eq!(members.len(), 2);
        // Both members should be at offset 0
        assert_eq!(members[0].offset, 0);
        assert_eq!(members[1].offset, 0);
    }

    #[test]
    fn test_void_pointer() {
        let mut table = TypeTable::new();
        let void_id = table.insert(TypeDef::Void);
        let void_ptr_id = table.insert(TypeDef::Pointer(void_id));

        assert_eq!(table.type_name(void_ptr_id), "void*");
        assert_eq!(table.type_size(void_ptr_id), Some(4));
    }

    #[test]
    fn test_double_pointer() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));
        let ptr1_id = table.insert(TypeDef::Pointer(int_id));
        let ptr2_id = table.insert(TypeDef::Pointer(ptr1_id));

        assert_eq!(table.type_name(ptr2_id), "int**");
        assert_eq!(table.type_size(ptr2_id), Some(4));
    }

    #[test]
    fn test_array_of_structs() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));

        let mut struct_def = StructDef::new(Some("Point".to_string()), 8, false);
        struct_def
            .members
            .push(MemberDef::new("x".to_string(), 0, int_id));
        struct_def
            .members
            .push(MemberDef::new("y".to_string(), 4, int_id));
        let struct_id = table.insert(TypeDef::Struct(struct_def));

        let array_id = table.insert(TypeDef::Array {
            element: struct_id,
            count: Some(5),
        });

        assert_eq!(table.type_name(array_id), "Point[5]");
        assert_eq!(table.type_size(array_id), Some(40)); // 5 * 8 bytes
    }

    #[test]
    fn test_variable_length_array() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));

        let array_id = table.insert(TypeDef::Array {
            element: int_id,
            count: None, // Unknown size
        });

        assert_eq!(table.type_name(array_id), "int[]");
        // Arrays without count have size 0, not None
        let size = table.type_size(array_id);
        assert!(size == Some(0) || size.is_none());
    }

    #[test]
    fn test_reference_type() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));
        let ref_id = table.insert(TypeDef::Reference(int_id));

        assert_eq!(table.type_name(ref_id), "int&");
        assert_eq!(table.type_size(ref_id), Some(4));
    }

    #[test]
    fn test_nested_structs() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));

        // Inner struct
        let mut inner = StructDef::new(Some("Inner".to_string()), 8, false);
        inner
            .members
            .push(MemberDef::new("a".to_string(), 0, int_id));
        inner
            .members
            .push(MemberDef::new("b".to_string(), 4, int_id));
        let inner_id = table.insert(TypeDef::Struct(inner));

        // Outer struct containing inner struct
        let mut outer = StructDef::new(Some("Outer".to_string()), 12, false);
        outer
            .members
            .push(MemberDef::new("inner".to_string(), 0, inner_id));
        outer
            .members
            .push(MemberDef::new("x".to_string(), 8, int_id));
        let outer_id = table.insert(TypeDef::Struct(outer));

        assert_eq!(table.type_size(outer_id), Some(12));

        let members = table.get_members(outer_id).unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].name, "inner");
        assert_eq!(members[0].type_id, inner_id);
    }

    #[test]
    fn test_const_volatile_qualifiers() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));
        let const_id = table.insert(TypeDef::Const(int_id));
        let volatile_id = table.insert(TypeDef::Volatile(int_id));

        // Qualifiers should be transparent for size
        assert_eq!(table.type_size(const_id), Some(4));
        assert_eq!(table.type_size(volatile_id), Some(4));

        // Names should reflect qualifiers
        assert!(table.type_name(const_id).contains("const"));
        assert!(table.type_name(volatile_id).contains("volatile"));
    }

    #[test]
    fn test_shared_type_table() {
        let mut table = TypeTable::new();
        let int_id = table.insert(TypeDef::Primitive(PrimitiveDef::Int));

        let shared = SharedTypeTable::new(table);

        // Should be able to clone and share
        let shared2 = shared.clone();

        assert_eq!(shared.type_name(int_id), "int");
        assert_eq!(shared2.type_name(int_id), "int");
    }
}
