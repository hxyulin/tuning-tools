//! DWARF Debug Info Parser using TypeTable
//!
//! This module parses DWARF debug information from ELF files and populates
//! a TypeTable with type definitions. It uses a simpler approach than the
//! original elf_parser.rs by leveraging the TypeTable's TypeId-based references.
//!
//! The parsing is done in a single pass with deferred resolution:
//! 1. Allocate TypeIds for all DIEs as we encounter them
//! 2. Parse type definitions, using TypeIds for references
//! 3. After all types are parsed, resolve forward declarations
//!
//! Template support is included by parsing DW_TAG_template_type_parameter
//! and DW_TAG_template_value_parameter DIEs.

use super::type_table::{
    BaseClassDef, DwarfTypeKey, EnumDef, EnumVariant, ForwardDeclKind, GlobalTypeKey, MemberDef,
    PrimitiveDef, SourceLocation, StructDef, TemplateParam, TypeDef, TypeId, TypeTable, VariantDef,
    VariantPart,
};
use gimli::{
    AttributeValue, DebuggingInformationEntry, Dwarf, EndianSlice, ReaderOffset, RunTimeEndian,
    Unit, UnitOffset,
};
use object::{Object, ObjectSection};
use std::borrow::Cow;

/// Status of a variable's address resolution
#[derive(Debug, Clone)]
pub enum VariableStatus {
    /// Variable has a valid static address
    Valid { address: u64 },
    /// Variable was optimized out (has type but no location)
    OptimizedOut,
    /// Variable is a local requiring runtime context
    LocalVariable { reason: String },
    /// Variable is an extern declaration (defined elsewhere)
    ExternDeclaration,
    /// Variable is a compile-time constant
    CompileTimeConstant { value: Option<i64> },
    /// Variable's type could not be resolved
    UnresolvedType { type_name: Option<String> },
    /// No location information in DWARF
    NoLocation,
    /// Location evaluated to address 0 (may be valid on embedded)
    AddressZero,
    /// Value is in a register only (no memory address)
    RegisterOnly { register: u16 },
    /// Value is implicit/computed (DW_OP_stack_value, DW_OP_implicit_value)
    ImplicitValue,
    /// Pointer to optimized-out data (DW_OP_implicit_pointer)
    ImplicitPointer,
    /// Variable split across multiple locations (DW_OP_piece)
    MultiPiece { has_address: bool },
    /// Compiler-generated variable (DW_AT_artificial)
    Artificial,
}

impl VariableStatus {
    /// Check if this variable is readable (has a valid address)
    pub fn is_readable(&self) -> bool {
        matches!(
            self,
            VariableStatus::Valid { .. }
                | VariableStatus::AddressZero
                | VariableStatus::MultiPiece { has_address: true }
        )
    }

    /// Get the address if available
    pub fn address(&self) -> Option<u64> {
        match self {
            VariableStatus::Valid { address } => Some(*address),
            VariableStatus::AddressZero => Some(0),
            VariableStatus::MultiPiece { has_address: true } => {
                // For multi-piece with address, we return address 0 as placeholder
                // The actual address handling happens during evaluation
                Some(0)
            }
            _ => None,
        }
    }

    /// Get a human-readable reason for the status
    pub fn reason(&self) -> &'static str {
        match self {
            VariableStatus::Valid { .. } => "Valid address",
            VariableStatus::OptimizedOut => "Optimized out by compiler",
            VariableStatus::LocalVariable { .. } => "Local variable (requires runtime context)",
            VariableStatus::ExternDeclaration => "External declaration (defined in another unit)",
            VariableStatus::CompileTimeConstant { .. } => {
                "Compile-time constant (no runtime storage)"
            }
            VariableStatus::UnresolvedType { .. } => "Type information unavailable",
            VariableStatus::NoLocation => "No location information in debug info",
            VariableStatus::AddressZero => "Address is 0 (may be valid on embedded targets)",
            VariableStatus::RegisterOnly { .. } => "Value in register only (no memory address)",
            VariableStatus::ImplicitValue => "Implicit value (computed, no storage)",
            VariableStatus::ImplicitPointer => "Implicit pointer (points to optimized-out data)",
            VariableStatus::MultiPiece { has_address: true } => {
                "Split across locations (partial address)"
            }
            VariableStatus::MultiPiece { has_address: false } => {
                "Split across registers/implicit values"
            }
            VariableStatus::Artificial => "Compiler-generated variable",
        }
    }

    /// Get detailed reason including any additional context
    pub fn detailed_reason(&self) -> String {
        match self {
            VariableStatus::LocalVariable { reason } => {
                format!("Local variable: {}", reason)
            }
            VariableStatus::CompileTimeConstant { value: Some(v) } => {
                format!("Compile-time constant: value = {}", v)
            }
            VariableStatus::UnresolvedType {
                type_name: Some(name),
            } => {
                format!("Unresolved type: {}", name)
            }
            _ => self.reason().to_string(),
        }
    }
}

/// Diagnostic statistics from DWARF parsing
#[derive(Debug, Default, Clone)]
pub struct DwarfDiagnostics {
    /// Total number of variables found in DWARF
    pub total_variables: usize,
    /// Variables with valid static addresses
    pub with_valid_address: usize,
    /// Variables optimized out by the compiler
    pub optimized_out: usize,
    /// Local variables requiring runtime context
    pub local_variables: usize,
    /// External declarations (defined elsewhere)
    pub extern_declarations: usize,
    /// Compile-time constants with no runtime storage
    pub compile_time_constants: usize,
    /// Variables with no location information
    pub no_location: usize,
    /// Variables with unresolved types
    pub unresolved_types: usize,
    /// Variables with address 0 (may be valid on embedded)
    pub address_zero: usize,
    /// Variables in registers only (no memory address)
    pub register_only: usize,
    /// Variables with implicit/computed values
    pub implicit_value: usize,
    /// Variables with implicit pointers
    pub implicit_pointer: usize,
    /// Variables split across multiple locations
    pub multi_piece: usize,
    /// Compiler-generated (artificial) variables
    pub artificial: usize,
}

impl DwarfDiagnostics {
    /// Increment the appropriate counter based on variable status
    pub fn record(&mut self, status: &VariableStatus) {
        self.total_variables += 1;
        match status {
            VariableStatus::Valid { .. } => self.with_valid_address += 1,
            VariableStatus::OptimizedOut => self.optimized_out += 1,
            VariableStatus::LocalVariable { .. } => self.local_variables += 1,
            VariableStatus::ExternDeclaration => self.extern_declarations += 1,
            VariableStatus::CompileTimeConstant { .. } => self.compile_time_constants += 1,
            VariableStatus::UnresolvedType { .. } => self.unresolved_types += 1,
            VariableStatus::NoLocation => self.no_location += 1,
            VariableStatus::AddressZero => self.address_zero += 1,
            VariableStatus::RegisterOnly { .. } => self.register_only += 1,
            VariableStatus::ImplicitValue => self.implicit_value += 1,
            VariableStatus::ImplicitPointer => self.implicit_pointer += 1,
            VariableStatus::MultiPiece { .. } => self.multi_piece += 1,
            VariableStatus::Artificial => self.artificial += 1,
        }
    }
}

/// A parsed symbol with type information
#[derive(Debug, Clone)]
pub struct ParsedSymbol {
    pub name: String,
    pub mangled_name: Option<String>,
    pub address: u64,
    pub size: u64,
    pub type_id: TypeId,
    pub is_global: bool,
    /// Status indicating why a variable can or cannot be read
    pub status: VariableStatus,
}

/// Result of parsing DWARF info
#[derive(Debug)]
pub struct DwarfParseResult {
    pub type_table: TypeTable,
    pub symbols: Vec<ParsedSymbol>,
    /// Mapping from variable name (including mangled names) to TypeId
    /// This includes variables without addresses that couldn't become full symbols
    pub name_to_type: std::collections::HashMap<String, TypeId>,
    /// Diagnostic statistics about variable parsing
    pub diagnostics: DwarfDiagnostics,
}

/// DWARF parser that populates a TypeTable
pub struct DwarfParser<'a, R: gimli::Reader> {
    dwarf: &'a Dwarf<R>,
    type_table: TypeTable,
    /// Parsed variables pending final resolution
    pending_symbols: Vec<PendingSymbol>,
    /// Cache of variable info by global DIE offset for abstract_origin/specification resolution
    variable_cache: std::collections::HashMap<usize, VariableInfo>,
    /// Variables needing deferred resolution (spec/origin DIE appeared before target)
    deferred_variables: Vec<DeferredVariable>,
    /// Whether the compile unit being walked is Rust (`DW_LANG_Rust`)
    unit_is_rust: bool,
}

/// Cached information about a variable (for abstract_origin/specification resolution)
#[derive(Debug, Clone)]
struct VariableInfo {
    name: Option<String>,
    linkage_name: Option<String>,
    type_key: Option<DwarfTypeKey>,
    size: u64,
    is_global: bool,
}

impl VariableInfo {
    /// Inherit missing fields from another VariableInfo (e.g., from abstract_origin target)
    fn inherit_from(&mut self, other: &VariableInfo) {
        if self.name.is_none() {
            self.name = other.name.clone();
        }
        if self.linkage_name.is_none() {
            self.linkage_name = other.linkage_name.clone();
        }
        if self.type_key.is_none() {
            self.type_key = other.type_key;
        }
        if self.size == 0 {
            self.size = other.size;
        }
        if !self.is_global {
            self.is_global = other.is_global;
        }
    }
}

/// A variable needing deferred resolution (target DIE appeared before this DIE)
#[derive(Debug)]
struct DeferredVariable {
    info: VariableInfo,
    status: VariableStatus,
    target_offset: usize,
}

/// A symbol pending type resolution
#[derive(Debug)]
struct PendingSymbol {
    name: String,
    mangled_name: Option<String>,
    size: u64,
    type_key: Option<DwarfTypeKey>,
    is_global: bool,
    status: VariableStatus,
}

type Reader<'a> = EndianSlice<'a, RunTimeEndian>;

impl<'a> DwarfParser<'a, Reader<'a>> {
    /// Parse DWARF info from raw bytes
    pub fn parse_bytes(data: &'a [u8]) -> Result<DwarfParseResult, String> {
        let file =
            object::File::parse(data).map_err(|e| format!("Failed to parse object: {}", e))?;

        let endian = if file.is_little_endian() {
            RunTimeEndian::Little
        } else {
            RunTimeEndian::Big
        };

        // Load DWARF sections
        let load_section = |id: gimli::SectionId| -> Result<Cow<[u8]>, gimli::Error> {
            Ok(file
                .section_by_name(id.name())
                .and_then(|s| s.data().ok())
                .map(Cow::Borrowed)
                .unwrap_or(Cow::Borrowed(&[])))
        };

        let dwarf_sections: gimli::DwarfSections<Cow<[u8]>> =
            gimli::DwarfSections::load(load_section)
                .map_err(|e| format!("Failed to load DWARF: {}", e))?;

        let dwarf = dwarf_sections.borrow(|section| EndianSlice::new(section, endian));

        let mut parser = DwarfParser {
            dwarf: &dwarf,
            type_table: TypeTable::new(),
            pending_symbols: Vec::new(),
            variable_cache: std::collections::HashMap::new(),
            deferred_variables: Vec::new(),
            unit_is_rust: false,
        };

        parser.parse_all_units()?;

        Ok(parser.finish())
    }

    /// Parse all compilation units
    fn parse_all_units(&mut self) -> Result<(), String> {
        let mut units = self.dwarf.units();
        let mut unit_index = 0usize;

        while let Some(header) = units
            .next()
            .map_err(|e| format!("Failed to read DWARF unit: {}", e))?
        {
            let unit = self
                .dwarf
                .unit(header)
                .map_err(|e| format!("Failed to parse DWARF unit: {}", e))?;

            self.parse_unit(&unit, unit_index)?;
            unit_index += 1;
        }

        Ok(())
    }

    /// Parse a single compilation unit
    fn parse_unit(&mut self, unit: &Unit<Reader<'a>>, unit_index: usize) -> Result<(), String> {
        self.unit_is_rust = unit
            .entries_tree(None)
            .ok()
            .and_then(|mut tree| {
                let root = tree.root().ok()?;
                root.entry().attr_value(gimli::DW_AT_language).ok()?
            })
            .is_some_and(|v| matches!(v, AttributeValue::Language(gimli::DW_LANG_Rust)));

        let mut entries = unit.entries();

        while let Some((_, entry)) = entries
            .next_dfs()
            .map_err(|e| format!("Failed to read DWARF entry: {}", e))?
        {
            self.parse_entry(unit, unit_index, entry)?;
        }

        Ok(())
    }

    /// Parse a single DIE
    fn parse_entry(
        &mut self,
        unit: &Unit<Reader<'a>>,
        unit_index: usize,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Result<(), String> {
        let offset: usize = entry.offset().0.into_u64() as usize;
        let key = DwarfTypeKey::new(unit_index, offset);

        // Also compute the global offset for this DIE so we can register it as an alias.
        // ARM compilers often use DebugInfoRef (global offsets) instead of UnitRef (local offsets).
        let global_offset = entry
            .offset()
            .to_debug_info_offset(&unit.header)
            .map(|o| o.0.into_u64() as usize);

        match entry.tag() {
            // Type DIEs
            gimli::DW_TAG_base_type => {
                self.parse_base_type(unit, key, entry);
                self.register_global_alias(key, global_offset);
            }
            gimli::DW_TAG_pointer_type => {
                self.parse_pointer_type(unit_index, key, entry);
                self.register_global_alias(key, global_offset);
            }
            gimli::DW_TAG_reference_type | gimli::DW_TAG_rvalue_reference_type => {
                self.parse_reference_type(unit_index, key, entry);
                self.register_global_alias(key, global_offset);
            }
            gimli::DW_TAG_const_type => {
                self.parse_const_type(unit_index, key, entry);
                self.register_global_alias(key, global_offset);
            }
            gimli::DW_TAG_volatile_type => {
                self.parse_volatile_type(unit_index, key, entry);
                self.register_global_alias(key, global_offset);
            }
            gimli::DW_TAG_restrict_type => {
                self.parse_restrict_type(unit_index, key, entry);
                self.register_global_alias(key, global_offset);
            }
            gimli::DW_TAG_typedef => {
                self.parse_typedef(unit, unit_index, key, entry);
                self.register_global_alias(key, global_offset);
            }
            gimli::DW_TAG_array_type => {
                self.parse_array_type(unit, unit_index, key, entry);
                self.register_global_alias(key, global_offset);
            }
            gimli::DW_TAG_structure_type | gimli::DW_TAG_class_type => {
                self.parse_struct_type(unit, unit_index, key, entry, false);
                self.register_global_alias(key, global_offset);
            }
            gimli::DW_TAG_union_type => {
                self.parse_struct_type(unit, unit_index, key, entry, true);
                self.register_global_alias(key, global_offset);
            }
            gimli::DW_TAG_enumeration_type => {
                self.parse_enum_type(unit, unit_index, key, entry);
                self.register_global_alias(key, global_offset);
            }
            gimli::DW_TAG_subroutine_type => {
                self.parse_subroutine_type(unit_index, key, entry);
                self.register_global_alias(key, global_offset);
            }
            gimli::DW_TAG_unspecified_type => {
                // Usually represents void
                self.type_table.insert_for_key(key, TypeDef::Void);
                self.register_global_alias(key, global_offset);
            }

            // Variable/symbol DIEs
            gimli::DW_TAG_variable => {
                self.parse_variable(unit, unit_index, key, entry);
                return Ok(());
            }

            _ => {
                // Ignore other tags
                return Ok(());
            }
        }

        // Every arm that reaches here defined a type under `key`
        if self.unit_is_rust {
            if let Some(id) = self.type_table.get_by_dwarf_key(key) {
                self.type_table.mark_rust(id);
                // rustc names base and pointer types (`u16`, `&mut T`, `fn() -> !`)
                if matches!(
                    entry.tag(),
                    gimli::DW_TAG_base_type | gimli::DW_TAG_pointer_type
                ) {
                    if let Some(name) = self.get_name(unit, entry) {
                        self.type_table.set_source_name(id, name);
                    }
                }
            }
        }

        Ok(())
    }

    /// Parse DW_TAG_base_type
    fn parse_base_type(
        &mut self,
        unit: &Unit<Reader<'a>>,
        key: DwarfTypeKey,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) {
        let name = self.get_name(unit, entry).unwrap_or_default();
        let size = self.get_byte_size(entry).unwrap_or(0);
        let encoding = self.get_encoding(entry);

        let prim = match encoding {
            Some(gimli::DW_ATE_boolean) => PrimitiveDef::Bool,
            Some(gimli::DW_ATE_signed_char) => PrimitiveDef::SignedChar,
            Some(gimli::DW_ATE_unsigned_char) => PrimitiveDef::UnsignedChar,
            Some(gimli::DW_ATE_signed) => match size {
                1 => PrimitiveDef::SignedChar,
                2 => PrimitiveDef::Short,
                4 => PrimitiveDef::Int,
                8 => PrimitiveDef::LongLong,
                _ => PrimitiveDef::SizedInt { size, signed: true },
            },
            Some(gimli::DW_ATE_unsigned) => match size {
                1 => PrimitiveDef::UnsignedChar,
                2 => PrimitiveDef::UnsignedShort,
                4 => PrimitiveDef::UnsignedInt,
                8 => PrimitiveDef::UnsignedLongLong,
                _ => PrimitiveDef::SizedInt {
                    size,
                    signed: false,
                },
            },
            Some(gimli::DW_ATE_float) => match size {
                4 => PrimitiveDef::Float,
                8 => PrimitiveDef::Double,
                16 => PrimitiveDef::LongDouble,
                _ => PrimitiveDef::SizedFloat { size },
            },
            Some(gimli::DW_ATE_UTF) => PrimitiveDef::UnicodeChar { size },
            _ => {
                // Try to infer from name
                match name.as_str() {
                    "bool" | "_Bool" => PrimitiveDef::Bool,
                    "char" => PrimitiveDef::Char,
                    "signed char" => PrimitiveDef::SignedChar,
                    "unsigned char" => PrimitiveDef::UnsignedChar,
                    "short" | "short int" => PrimitiveDef::Short,
                    "unsigned short" | "short unsigned int" => PrimitiveDef::UnsignedShort,
                    "int" => PrimitiveDef::Int,
                    "unsigned int" | "unsigned" => PrimitiveDef::UnsignedInt,
                    "long" | "long int" => PrimitiveDef::Long,
                    "unsigned long" | "long unsigned int" => PrimitiveDef::UnsignedLong,
                    "long long" | "long long int" => PrimitiveDef::LongLong,
                    "unsigned long long" | "long long unsigned int" => {
                        PrimitiveDef::UnsignedLongLong
                    }
                    "float" => PrimitiveDef::Float,
                    "double" => PrimitiveDef::Double,
                    "long double" => PrimitiveDef::LongDouble,
                    _ => PrimitiveDef::SizedInt { size, signed: true },
                }
            }
        };

        self.type_table
            .insert_for_key(key, TypeDef::Primitive(prim));
    }

    /// Parse DW_TAG_pointer_type
    fn parse_pointer_type(
        &mut self,
        unit_index: usize,
        key: DwarfTypeKey,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) {
        let inner_id = if let Some(ref_key) = self.get_type_ref_key(unit_index, entry) {
            self.type_table.get_or_allocate(ref_key.to_dwarf_key())
        } else {
            // Void pointer
            self.ensure_void_type()
        };

        self.type_table
            .insert_for_key(key, TypeDef::Pointer(inner_id));
    }

    /// Parse DW_TAG_reference_type
    fn parse_reference_type(
        &mut self,
        unit_index: usize,
        key: DwarfTypeKey,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) {
        let inner_id = if let Some(ref_key) = self.get_type_ref_key(unit_index, entry) {
            self.type_table.get_or_allocate(ref_key.to_dwarf_key())
        } else {
            self.ensure_void_type()
        };

        self.type_table
            .insert_for_key(key, TypeDef::Reference(inner_id));
    }

    /// Parse DW_TAG_const_type
    fn parse_const_type(
        &mut self,
        unit_index: usize,
        key: DwarfTypeKey,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) {
        let inner_id = if let Some(ref_key) = self.get_type_ref_key(unit_index, entry) {
            self.type_table.get_or_allocate(ref_key.to_dwarf_key())
        } else {
            self.ensure_void_type()
        };

        self.type_table
            .insert_for_key(key, TypeDef::Const(inner_id));
    }

    /// Parse DW_TAG_volatile_type
    fn parse_volatile_type(
        &mut self,
        unit_index: usize,
        key: DwarfTypeKey,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) {
        let inner_id = if let Some(ref_key) = self.get_type_ref_key(unit_index, entry) {
            self.type_table.get_or_allocate(ref_key.to_dwarf_key())
        } else {
            self.ensure_void_type()
        };

        self.type_table
            .insert_for_key(key, TypeDef::Volatile(inner_id));
    }

    /// Parse DW_TAG_restrict_type
    fn parse_restrict_type(
        &mut self,
        unit_index: usize,
        key: DwarfTypeKey,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) {
        let inner_id = if let Some(ref_key) = self.get_type_ref_key(unit_index, entry) {
            self.type_table.get_or_allocate(ref_key.to_dwarf_key())
        } else {
            self.ensure_void_type()
        };

        self.type_table
            .insert_for_key(key, TypeDef::Restrict(inner_id));
    }

    /// Parse DW_TAG_typedef
    fn parse_typedef(
        &mut self,
        unit: &Unit<Reader<'a>>,
        unit_index: usize,
        key: DwarfTypeKey,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) {
        let name = self.get_name(unit, entry).unwrap_or_default();

        let underlying = if let Some(ref_key) = self.get_type_ref_key(unit_index, entry) {
            self.type_table.get_or_allocate(ref_key.to_dwarf_key())
        } else {
            self.ensure_void_type()
        };

        self.type_table
            .insert_for_key(key, TypeDef::Typedef { name, underlying });
    }

    /// Parse DW_TAG_array_type
    fn parse_array_type(
        &mut self,
        unit: &Unit<Reader<'a>>,
        unit_index: usize,
        key: DwarfTypeKey,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) {
        let element_id = if let Some(ref_key) = self.get_type_ref_key(unit_index, entry) {
            self.type_table.get_or_allocate(ref_key.to_dwarf_key())
        } else {
            self.ensure_void_type()
        };

        // Get array count from subrange child
        let count = self.get_array_count(unit, entry);

        self.type_table.insert_for_key(
            key,
            TypeDef::Array {
                element: element_id,
                count,
            },
        );
    }

    /// Parse DW_TAG_structure_type, DW_TAG_class_type, or DW_TAG_union_type
    fn parse_struct_type(
        &mut self,
        unit: &Unit<Reader<'a>>,
        unit_index: usize,
        key: DwarfTypeKey,
        entry: &DebuggingInformationEntry<Reader<'a>>,
        is_union: bool,
    ) {
        let name = self.get_name(unit, entry);
        let size = self.get_byte_size(entry).unwrap_or(0);
        let is_class = entry.tag() == gimli::DW_TAG_class_type;
        let is_declaration = self.has_declaration_attr(entry);

        // Check if this is a forward declaration
        if is_declaration {
            let kind = if is_union {
                ForwardDeclKind::Union
            } else if is_class {
                ForwardDeclKind::Class
            } else {
                ForwardDeclKind::Struct
            };

            self.type_table.insert_for_key(
                key,
                TypeDef::ForwardDecl {
                    name: name.unwrap_or_default(),
                    kind,
                    target: None,
                },
            );
            return;
        }

        // Pre-allocate the TypeId so members can reference it (for recursive types)
        let struct_id = self.type_table.get_or_allocate(key);

        let mut struct_def = StructDef::new(name, size, is_class);

        // Parse children (members, base classes, template params)
        if let Ok(mut tree) = unit.entries_tree(Some(entry.offset())) {
            if let Ok(root) = tree.root() {
                let mut children = root.children();
                while let Ok(Some(child)) = children.next() {
                    let child_entry = child.entry();
                    match child_entry.tag() {
                        gimli::DW_TAG_member => {
                            if let Some(member) = self.parse_member(unit, unit_index, child_entry) {
                                struct_def.members.push(member);
                            }
                        }
                        gimli::DW_TAG_inheritance => {
                            if let Some(base) =
                                self.parse_inheritance(unit, unit_index, child_entry)
                            {
                                struct_def.base_classes.push(base);
                            }
                        }
                        gimli::DW_TAG_template_type_parameter => {
                            if let Some(param) =
                                self.parse_template_type_param(unit, unit_index, child_entry)
                            {
                                struct_def.template_params.push(param);
                            }
                        }
                        gimli::DW_TAG_template_value_parameter => {
                            if let Some(param) = self.parse_template_value_param(unit, child_entry)
                            {
                                struct_def.template_params.push(param);
                            }
                        }
                        gimli::DW_TAG_variant_part => {
                            struct_def.variant_part =
                                Some(self.parse_variant_part(unit, unit_index, child));
                        }
                        _ => {}
                    }
                }
            }
        }

        let def = if is_union {
            TypeDef::Union(struct_def)
        } else {
            TypeDef::Struct(struct_def)
        };

        self.type_table.define(struct_id, def);
    }

    /// Parse a Rust enum's DW_TAG_variant_part: one discriminant member plus
    /// DW_TAG_variant children, each holding a single member for its payload.
    fn parse_variant_part(
        &mut self,
        unit: &Unit<Reader<'a>>,
        unit_index: usize,
        node: gimli::EntriesTreeNode<Reader<'a>>,
    ) -> VariantPart {
        let discr_ref = match node.entry().attr_value(gimli::DW_AT_discr) {
            Ok(Some(AttributeValue::UnitRef(offset))) => Some(offset),
            _ => None,
        };

        let mut part = VariantPart::default();
        let mut members: Vec<(UnitOffset, MemberDef)> = Vec::new();
        let mut children = node.children();
        while let Ok(Some(child)) = children.next() {
            let entry = child.entry();
            match entry.tag() {
                gimli::DW_TAG_member => {
                    let offset = entry.offset();
                    if let Some(member) = self.parse_member(unit, unit_index, entry) {
                        members.push((offset, member));
                    }
                }
                gimli::DW_TAG_variant => {
                    let discr_value = entry
                        .attr_value(gimli::DW_AT_discr_value)
                        .ok()
                        .flatten()
                        .and_then(|v| {
                            v.udata_value()
                                .or_else(|| v.sdata_value().map(|s| s as u64))
                        });
                    let mut grandchildren = child.children();
                    while let Ok(Some(gc)) = grandchildren.next() {
                        if gc.entry().tag() == gimli::DW_TAG_member {
                            if let Some(member) = self.parse_member(unit, unit_index, gc.entry()) {
                                part.variants.push(VariantDef {
                                    discr_value,
                                    member,
                                    location: self.decl_location(unit, gc.entry()),
                                });
                            }
                            break;
                        }
                    }
                }
                _ => {}
            }
        }

        part.discriminant = match discr_ref {
            Some(target) => members
                .into_iter()
                .find(|(o, _)| *o == target)
                .map(|(_, m)| m),
            None => members.into_iter().next().map(|(_, m)| m),
        };
        part
    }

    /// Parse DW_TAG_member
    fn parse_member(
        &mut self,
        unit: &Unit<Reader<'a>>,
        unit_index: usize,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<MemberDef> {
        let name = self.get_name(unit, entry).unwrap_or_default();
        let offset = self.get_member_offset(unit, entry).unwrap_or(0);

        let type_id = if let Some(ref_key) = self.get_type_ref_key(unit_index, entry) {
            self.type_table.get_or_allocate(ref_key.to_dwarf_key())
        } else {
            TypeId::INVALID
        };

        let mut member = MemberDef::new(name, offset, type_id);
        member.bit_offset = self.get_bit_offset(entry);
        member.bit_size = self.get_bit_size(entry);

        Some(member)
    }

    /// Parse DW_TAG_inheritance
    fn parse_inheritance(
        &mut self,
        unit: &Unit<Reader<'a>>,
        unit_index: usize,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<BaseClassDef> {
        let ref_key = self.get_type_ref_key(unit_index, entry)?;
        let base_type_id = self.type_table.get_or_allocate(ref_key.to_dwarf_key());

        let offset = self.get_member_offset(unit, entry).unwrap_or(0);
        let is_virtual = self.get_virtuality(entry).is_some();

        Some(BaseClassDef {
            type_id: base_type_id,
            offset,
            is_virtual,
        })
    }

    /// Parse DW_TAG_template_type_parameter
    fn parse_template_type_param(
        &mut self,
        unit: &Unit<Reader<'a>>,
        unit_index: usize,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<TemplateParam> {
        let name = self
            .get_name(unit, entry)
            .unwrap_or_else(|| "T".to_string());

        let type_id = if let Some(type_offset) = self.get_type_ref(entry) {
            let ref_key = DwarfTypeKey::new(unit_index, type_offset.0.into_u64() as usize);
            self.type_table.get_or_allocate(ref_key)
        } else {
            self.ensure_void_type()
        };

        Some(TemplateParam::Type { name, type_id })
    }

    /// Parse DW_TAG_template_value_parameter
    fn parse_template_value_param(
        &mut self,
        unit: &Unit<Reader<'a>>,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<TemplateParam> {
        let name = self
            .get_name(unit, entry)
            .unwrap_or_else(|| "N".to_string());
        let value = self.get_const_value(entry).unwrap_or(0);

        Some(TemplateParam::Value { name, value })
    }

    /// Parse DW_TAG_enumeration_type
    fn parse_enum_type(
        &mut self,
        unit: &Unit<Reader<'a>>,
        unit_index: usize,
        key: DwarfTypeKey,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) {
        let name = self.get_name(unit, entry);
        let size = self.get_byte_size(entry).unwrap_or(4);
        let is_declaration = self.has_declaration_attr(entry);

        // Check if this is a forward declaration
        if is_declaration {
            self.type_table.insert_for_key(
                key,
                TypeDef::ForwardDecl {
                    name: name.unwrap_or_default(),
                    kind: ForwardDeclKind::Enum,
                    target: None,
                },
            );
            return;
        }

        // Check for C++11 enum class (DW_AT_enum_class)
        let is_scoped = self.has_enum_class_attr(entry);

        // Get underlying type if specified
        let underlying_type = if let Some(type_offset) = self.get_type_ref(entry) {
            let ref_key = DwarfTypeKey::new(unit_index, type_offset.0.into_u64() as usize);
            Some(self.type_table.get_or_allocate(ref_key))
        } else {
            None
        };

        let mut enum_def = EnumDef::new(name, size, is_scoped);
        enum_def.underlying_type = underlying_type;

        // Parse enumerator children
        if let Ok(mut tree) = unit.entries_tree(Some(entry.offset())) {
            if let Ok(root) = tree.root() {
                let mut children = root.children();
                while let Ok(Some(child)) = children.next() {
                    if child.entry().tag() == gimli::DW_TAG_enumerator {
                        if let Some(variant) = self.parse_enumerator(unit, child.entry()) {
                            enum_def.variants.push(variant);
                        }
                    }
                }
            }
        }

        self.type_table.insert_for_key(key, TypeDef::Enum(enum_def));
    }

    /// Parse DW_TAG_enumerator
    fn parse_enumerator(
        &mut self,
        unit: &Unit<Reader<'a>>,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<EnumVariant> {
        let name = self.get_name(unit, entry)?;
        let value = self.get_const_value(entry).unwrap_or(0);
        Some(EnumVariant { name, value })
    }

    /// Parse DW_TAG_subroutine_type
    fn parse_subroutine_type(
        &mut self,
        unit_index: usize,
        key: DwarfTypeKey,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) {
        let return_type = if let Some(ref_key) = self.get_type_ref_key(unit_index, entry) {
            Some(self.type_table.get_or_allocate(ref_key.to_dwarf_key()))
        } else {
            None // void return
        };

        // Note: We could parse parameter types from children, but for now keep it simple
        self.type_table.insert_for_key(
            key,
            TypeDef::Subroutine {
                return_type,
                params: Vec::new(),
            },
        );
    }

    /// Parse DW_TAG_variable
    fn parse_variable(
        &mut self,
        unit: &Unit<Reader<'a>>,
        unit_index: usize,
        _key: DwarfTypeKey,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) {
        let global_offset = self.get_global_offset(unit, entry);

        // Extract variable info from this DIE
        let mut info = VariableInfo {
            name: self.get_name(unit, entry),
            linkage_name: self.get_linkage_name(unit, entry),
            type_key: self
                .get_type_ref_key(unit_index, entry)
                .map(|k| k.to_dwarf_key()),
            size: self.get_byte_size(entry).unwrap_or(0),
            is_global: self.has_external_attr(entry),
        };

        // Cache variables with type info for abstract_origin/specification resolution
        if info.type_key.is_some() {
            if let Some(off) = global_offset {
                self.variable_cache.insert(off, info.clone());
            }
        }

        // Pure declarations shouldn't become symbols - they're referenced via specification
        if self.has_declaration_attr(entry) {
            return;
        }

        // Get variable status (includes address if available)
        let status = self.get_variable_status(unit, entry);

        // Check for DW_AT_specification or DW_AT_abstract_origin and inherit info
        let target_offset = self.get_reference_offset(unit, entry);
        if let Some(target_off) = target_offset {
            if let Some(target_info) = self.variable_cache.get(&target_off) {
                info.inherit_from(target_info);
            } else {
                // Target not yet parsed - defer to second pass
                self.deferred_variables.push(DeferredVariable {
                    info,
                    status,
                    target_offset: target_off,
                });
                return;
            }
        }

        // Create pending symbol if we have enough info
        self.try_add_pending_symbol(info, status);
    }

    /// Get the global .debug_info offset for a DIE
    fn get_global_offset(
        &self,
        unit: &Unit<Reader<'a>>,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<usize> {
        entry
            .offset()
            .to_debug_info_offset(&unit.header)
            .map(|o| o.0.into_u64() as usize)
    }

    /// Get the target offset from DW_AT_specification or DW_AT_abstract_origin
    fn get_reference_offset(
        &self,
        unit: &Unit<Reader<'a>>,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<usize> {
        // Try DW_AT_specification first, then DW_AT_abstract_origin
        for attr_name in [gimli::DW_AT_specification, gimli::DW_AT_abstract_origin] {
            if let Ok(Some(attr)) = entry.attr_value(attr_name) {
                let offset = match attr {
                    AttributeValue::UnitRef(offset) => offset
                        .to_debug_info_offset(&unit.header)
                        .map(|o| o.0.into_u64() as usize),
                    AttributeValue::DebugInfoRef(offset) => Some(offset.0.into_u64() as usize),
                    _ => None,
                };
                if offset.is_some() {
                    return offset;
                }
            }
        }
        None
    }

    /// Try to create a pending symbol from variable info
    fn try_add_pending_symbol(&mut self, info: VariableInfo, status: VariableStatus) {
        let name = info.linkage_name.clone().or(info.name.clone());
        let name = match name {
            Some(n) if !n.is_empty() => n,
            _ => return, // Skip anonymous variables
        };

        if info.type_key.is_none() {
            return; // Skip variables without type info
        }

        self.pending_symbols.push(PendingSymbol {
            name: info.name.unwrap_or_else(|| name.clone()),
            mangled_name: if info.linkage_name.as_ref() != Some(&name) {
                info.linkage_name
            } else {
                None
            },
            size: info.size,
            type_key: info.type_key,
            is_global: info.is_global,
            status,
        });
    }

    /// Finish parsing and return the result
    fn finish(mut self) -> DwarfParseResult {
        // Resolve pending specifications (second pass)
        // These are specifications that referenced declarations that appeared later in DWARF
        self.resolve_pending_specifications();

        // Resolve forward declarations
        self.type_table.resolve_forward_declarations();

        // Resolve inherited members
        self.resolve_inherited_members();

        // Build name-to-type mapping for ALL variables (including those without addresses)
        // and separate out symbols with addresses
        let mut name_to_type = std::collections::HashMap::new();
        let mut symbols = Vec::new();
        let mut diagnostics = DwarfDiagnostics::default();

        for pending in self.pending_symbols {
            let type_id = pending
                .type_key
                .and_then(|k| self.type_table.get_by_dwarf_key(k))
                .unwrap_or(TypeId::INVALID);

            // Track diagnostics based on status
            diagnostics.record(&pending.status);

            if !type_id.is_valid() {
                diagnostics.unresolved_types += 1;
                continue;
            }

            // Add to name-to-type mapping (for all variables)
            name_to_type.insert(pending.name.clone(), type_id);
            if let Some(ref mangled) = pending.mangled_name {
                name_to_type.insert(mangled.clone(), type_id);
            }

            // Get address from status (if readable)
            let address = pending.status.address().unwrap_or(0);

            // Get size from type if not specified
            let size = if pending.size > 0 {
                pending.size
            } else {
                self.type_table.type_size(type_id).unwrap_or(0)
            };

            symbols.push(ParsedSymbol {
                name: pending.name,
                mangled_name: pending.mangled_name,
                address,
                size,
                type_id,
                is_global: pending.is_global,
                status: pending.status,
            });
        }

        let stats = self.type_table.stats();

        tracing::debug!(
            "DWARF parsing complete: {} types, {} structs, {} forward decls ({} unresolved)",
            stats.total_types,
            stats.structs,
            stats.forward_decls,
            stats.unresolved_forward_decls,
        );

        // Log detailed diagnostics
        tracing::info!(
            "DWARF variables: {} total | {} readable | {} optimized | {} register-only | {} artificial",
            diagnostics.total_variables,
            diagnostics.with_valid_address + diagnostics.address_zero,
            diagnostics.optimized_out,
            diagnostics.register_only,
            diagnostics.artificial,
        );

        if diagnostics.local_variables > 0
            || diagnostics.extern_declarations > 0
            || diagnostics.compile_time_constants > 0
        {
            tracing::debug!(
                "DWARF details: {} local | {} extern | {} const",
                diagnostics.local_variables,
                diagnostics.extern_declarations,
                diagnostics.compile_time_constants,
            );
        }

        if diagnostics.no_location > 0
            || diagnostics.unresolved_types > 0
            || diagnostics.implicit_value > 0
            || diagnostics.implicit_pointer > 0
            || diagnostics.multi_piece > 0
        {
            tracing::debug!(
                "DWARF edge cases: {} no location | {} unresolved types | {} implicit | {} implicit ptr | {} multi-piece | {} addr zero",
                diagnostics.no_location,
                diagnostics.unresolved_types,
                diagnostics.implicit_value,
                diagnostics.implicit_pointer,
                diagnostics.multi_piece,
                diagnostics.address_zero,
            );
        }

        // Log info about variable cache and deferred resolution
        tracing::debug!(
            "DWARF variable cache: {} entries, {} deferred resolutions",
            self.variable_cache.len(),
            self.deferred_variables.len()
        );

        DwarfParseResult {
            type_table: self.type_table,
            symbols,
            name_to_type,
            diagnostics,
        }
    }

    /// Resolve deferred variables that couldn't be resolved on first pass
    fn resolve_pending_specifications(&mut self) {
        let deferred = std::mem::take(&mut self.deferred_variables);

        for var in deferred {
            let mut info = var.info;

            // Try to find the target now
            if let Some(target_info) = self.variable_cache.get(&var.target_offset) {
                info.inherit_from(target_info);
            }

            self.try_add_pending_symbol(info, var.status);
        }
    }

    /// Resolve inherited members from base classes
    fn resolve_inherited_members(&mut self) {
        // Collect all struct/union type IDs
        let struct_ids: Vec<TypeId> = self
            .type_table
            .all_type_ids()
            .filter(|&id| {
                matches!(
                    self.type_table.get(id),
                    Some(TypeDef::Struct(_)) | Some(TypeDef::Union(_))
                )
            })
            .collect();

        for id in struct_ids {
            self.resolve_inherited_members_for(id, &mut std::collections::HashSet::new());
        }
    }

    fn resolve_inherited_members_for(
        &mut self,
        id: TypeId,
        visited: &mut std::collections::HashSet<TypeId>,
    ) {
        if visited.contains(&id) {
            return; // Prevent infinite recursion
        }
        visited.insert(id);

        // Get base classes for this struct
        let base_classes: Vec<BaseClassDef> = match self.type_table.get(id) {
            Some(TypeDef::Struct(s)) | Some(TypeDef::Union(s)) => s.base_classes.clone(),
            _ => return,
        };

        if base_classes.is_empty() {
            return;
        }

        // Collect members from base classes
        let mut inherited_members = Vec::new();
        for base in &base_classes {
            // First resolve the base class's inherited members
            let resolved_base_id = self.type_table.resolve(base.type_id);
            self.resolve_inherited_members_for(resolved_base_id, visited);

            // Then collect its members
            if let Some(members) = self.type_table.get_members(resolved_base_id) {
                for member in members {
                    inherited_members.push(MemberDef {
                        name: member.name.clone(),
                        offset: member.offset + base.offset,
                        type_id: member.type_id,
                        bit_offset: member.bit_offset,
                        bit_size: member.bit_size,
                    });
                }
            }
        }

        // Add inherited members to the struct
        if !inherited_members.is_empty() {
            if let Some(TypeDef::Struct(s)) | Some(TypeDef::Union(s)) = self.type_table.get_mut(id)
            {
                // Insert inherited members at the beginning
                inherited_members.append(&mut s.members);
                s.members = inherited_members;
            }
        }
    }

    /// Ensure we have a void type and return its ID
    fn ensure_void_type(&mut self) -> TypeId {
        // Look for an existing void type
        for (id, def) in self.type_table.iter() {
            if matches!(def, TypeDef::Void) {
                return id;
            }
        }
        // Create one if not found
        self.type_table.insert(TypeDef::Void)
    }

    // ==================== Attribute Helpers ====================

    fn get_name(
        &self,
        unit: &Unit<Reader<'a>>,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<String> {
        let attr = entry.attr_value(gimli::DW_AT_name).ok()??;
        self.attr_to_string(unit, &attr)
    }

    /// `DW_AT_decl_file` and `DW_AT_decl_line`, the file named through the
    /// unit's line program.
    fn decl_location(
        &self,
        unit: &Unit<Reader<'a>>,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<SourceLocation> {
        let line = entry
            .attr_value(gimli::DW_AT_decl_line)
            .ok()??
            .udata_value()?;
        let index = match entry.attr_value(gimli::DW_AT_decl_file).ok()?? {
            AttributeValue::FileIndex(i) => i,
            other => other.udata_value()?,
        };
        let header = unit.line_program.as_ref()?.header();
        let file = header.file(index)?;
        // Line-table strings may sit in `.debug_line_str`, which only
        // `Dwarf::attr_string` reads
        let text = |attr| {
            self.dwarf
                .attr_string(unit, attr)
                .ok()
                .map(|s| s.to_string_lossy().into_owned())
        };
        let name = text(file.path_name())?;
        let path = match file.directory(header).and_then(text) {
            Some(dir) if !dir.is_empty() && !name.starts_with('/') => {
                format!("{}/{name}", dir.trim_end_matches('/'))
            }
            _ => name,
        };
        Some(SourceLocation { file: path, line })
    }

    fn get_linkage_name(
        &self,
        unit: &Unit<Reader<'a>>,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<String> {
        // Try DW_AT_linkage_name first, then DW_AT_MIPS_linkage_name
        let attr = entry
            .attr_value(gimli::DW_AT_linkage_name)
            .ok()?
            .or_else(|| entry.attr_value(gimli::DW_AT_MIPS_linkage_name).ok()?);
        attr.and_then(|a| self.attr_to_string(unit, &a))
    }

    fn attr_to_string(
        &self,
        unit: &Unit<Reader<'a>>,
        attr: &AttributeValue<Reader<'a>>,
    ) -> Option<String> {
        match attr {
            AttributeValue::DebugStrRef(offset) => self
                .dwarf
                .debug_str
                .get_str(*offset)
                .ok()
                .map(|s| s.to_string_lossy().to_string()),
            AttributeValue::String(s) => Some(s.to_string_lossy().to_string()),
            AttributeValue::DebugStrOffsetsIndex(_index) => self
                .dwarf
                .attr_string(unit, *attr)
                .ok()
                .map(|s| s.to_string_lossy().to_string()),
            _ => None,
        }
    }

    fn get_byte_size(&self, entry: &DebuggingInformationEntry<Reader<'a>>) -> Option<u64> {
        match entry.attr_value(gimli::DW_AT_byte_size).ok()?? {
            AttributeValue::Udata(size) => Some(size),
            AttributeValue::Data1(size) => Some(size as u64),
            AttributeValue::Data2(size) => Some(size as u64),
            AttributeValue::Data4(size) => Some(size as u64),
            AttributeValue::Data8(size) => Some(size),
            _ => None,
        }
    }

    fn get_encoding(&self, entry: &DebuggingInformationEntry<Reader<'a>>) -> Option<gimli::DwAte> {
        match entry.attr_value(gimli::DW_AT_encoding).ok()?? {
            AttributeValue::Encoding(enc) => Some(enc),
            _ => None,
        }
    }

    /// Register a type under its global .debug_info offset as an alias.
    /// This allows lookups via DebugInfoRef to find types that were parsed
    /// with unit-local keys.
    fn register_global_alias(&mut self, local_key: DwarfTypeKey, global_offset: Option<usize>) {
        if let Some(global_off) = global_offset {
            // Get the TypeId that was registered for the local key
            if let Some(type_id) = self.type_table.get_by_dwarf_key(local_key) {
                // Register the same TypeId under the global key (using usize::MAX as sentinel)
                let global_key = GlobalTypeKey::global(global_off).to_dwarf_key();
                self.type_table.register_alias(global_key, type_id);
            }
        }
    }

    /// Get a type reference from DW_AT_type attribute.
    /// Returns a GlobalTypeKey that can handle both unit-local refs (UnitRef) and
    /// global refs (DebugInfoRef) which ARM compilers commonly use.
    fn get_type_ref_key(
        &self,
        unit_index: usize,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<GlobalTypeKey> {
        match entry.attr_value(gimli::DW_AT_type).ok()?? {
            // Unit-local reference (common in GCC)
            AttributeValue::UnitRef(offset) => Some(GlobalTypeKey::unit_local(
                unit_index,
                offset.0.into_u64() as usize,
            )),
            // Global reference in .debug_info (common in ARM compilers)
            AttributeValue::DebugInfoRef(offset) => {
                Some(GlobalTypeKey::global(offset.0.into_u64() as usize))
            }
            _ => None,
        }
    }

    /// Legacy helper - get type ref as UnitOffset (for backwards compatibility where needed)
    fn get_type_ref(&self, entry: &DebuggingInformationEntry<Reader<'a>>) -> Option<UnitOffset> {
        match entry.attr_value(gimli::DW_AT_type).ok()?? {
            AttributeValue::UnitRef(offset) => Some(offset),
            _ => None,
        }
    }

    fn get_member_offset(
        &self,
        unit: &Unit<Reader<'a>>,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<u64> {
        match entry.attr_value(gimli::DW_AT_data_member_location).ok()?? {
            // Direct offset values (common in DWARF 4+, GCC)
            AttributeValue::Udata(offset) => Some(offset),
            AttributeValue::Data1(offset) => Some(offset as u64),
            AttributeValue::Data2(offset) => Some(offset as u64),
            AttributeValue::Data4(offset) => Some(offset as u64),
            AttributeValue::Data8(offset) => Some(offset),
            AttributeValue::Sdata(offset) => Some(offset as u64),
            // Expression-based offset (common in DWARF 3, ARM Compiler 5)
            // ARM Compiler uses DW_OP_plus_uconst expressions like "23 N" meaning offset N
            AttributeValue::Exprloc(expr) => self.evaluate_member_offset_expr(unit, &expr),
            // Block form (older DWARF)
            AttributeValue::Block(block) => {
                let expr = gimli::Expression(block);
                self.evaluate_member_offset_expr(unit, &expr)
            }
            _ => Some(0),
        }
    }

    /// Evaluate a member offset expression (typically DW_OP_plus_uconst)
    fn evaluate_member_offset_expr(
        &self,
        unit: &Unit<Reader<'a>>,
        expr: &gimli::Expression<Reader<'a>>,
    ) -> Option<u64> {
        let mut ops = (*expr).operations(unit.encoding());

        // Look for DW_OP_plus_uconst which is the common case for member offsets
        // The expression is evaluated with an implicit "base address" on the stack,
        // and DW_OP_plus_uconst adds the offset to it.
        while let Ok(Some(op)) = ops.next() {
            match op {
                // DW_OP_plus_uconst: adds a constant to top of stack
                // For member offsets, this IS the offset value
                gimli::Operation::PlusConstant { value } => {
                    return Some(value);
                }
                // DW_OP_constu followed by DW_OP_plus
                gimli::Operation::UnsignedConstant { value } => {
                    // Check if next op is Plus
                    if let Ok(Some(gimli::Operation::Plus)) = ops.next() {
                        return Some(value);
                    }
                    // Otherwise this might be the offset directly
                    return Some(value);
                }
                // Direct address/literal (less common but possible)
                gimli::Operation::Address { address } => {
                    return Some(address);
                }
                _ => continue,
            }
        }

        // Default to 0 if we can't parse the expression
        Some(0)
    }

    fn get_bit_offset(&self, entry: &DebuggingInformationEntry<Reader<'a>>) -> Option<u64> {
        // Try DW_AT_data_bit_offset first (DWARF 4+)
        if let Ok(Some(attr)) = entry.attr_value(gimli::DW_AT_data_bit_offset) {
            return match attr {
                AttributeValue::Udata(v) => Some(v),
                AttributeValue::Data1(v) => Some(v as u64),
                AttributeValue::Data2(v) => Some(v as u64),
                AttributeValue::Data4(v) => Some(v as u64),
                AttributeValue::Data8(v) => Some(v),
                _ => None,
            };
        }

        // Fall back to DW_AT_bit_offset (DWARF 3)
        match entry.attr_value(gimli::DW_AT_bit_offset).ok()?? {
            AttributeValue::Udata(v) => Some(v),
            AttributeValue::Data1(v) => Some(v as u64),
            AttributeValue::Data2(v) => Some(v as u64),
            AttributeValue::Data4(v) => Some(v as u64),
            AttributeValue::Data8(v) => Some(v),
            _ => None,
        }
    }

    fn get_bit_size(&self, entry: &DebuggingInformationEntry<Reader<'a>>) -> Option<u64> {
        match entry.attr_value(gimli::DW_AT_bit_size).ok()?? {
            AttributeValue::Udata(v) => Some(v),
            AttributeValue::Data1(v) => Some(v as u64),
            AttributeValue::Data2(v) => Some(v as u64),
            AttributeValue::Data4(v) => Some(v as u64),
            AttributeValue::Data8(v) => Some(v),
            _ => None,
        }
    }

    fn get_const_value(&self, entry: &DebuggingInformationEntry<Reader<'a>>) -> Option<i64> {
        match entry.attr_value(gimli::DW_AT_const_value).ok()?? {
            AttributeValue::Sdata(v) => Some(v),
            AttributeValue::Udata(v) => Some(v as i64),
            AttributeValue::Data1(v) => Some(v as i64),
            AttributeValue::Data2(v) => Some(v as i64),
            AttributeValue::Data4(v) => Some(v as i64),
            AttributeValue::Data8(v) => Some(v as i64),
            _ => None,
        }
    }

    /// Get detailed variable status including why a variable can or cannot be read.
    ///
    /// This provides diagnostic information about:
    /// - Artificial (compiler-generated) variables
    /// - Compile-time constants (have value but no runtime storage)
    /// - External declarations (defined in another compilation unit)
    /// - Optimized out variables (no location in DWARF)
    /// - Local variables (require runtime context like registers or stack)
    /// - Valid static addresses (including address 0 on embedded targets)
    fn get_variable_status(
        &self,
        unit: &Unit<Reader<'a>>,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> VariableStatus {
        // 0. Check for artificial (compiler-generated) FIRST
        // These include: `this` pointers, VLA bounds, closure captures, temporaries
        if self.has_artificial_attr(entry) {
            return VariableStatus::Artificial;
        }

        // 1. Check for compile-time constant (has value but may not have location)
        let has_const_value = entry
            .attr_value(gimli::DW_AT_const_value)
            .ok()
            .flatten()
            .is_some();
        let has_location = entry
            .attr_value(gimli::DW_AT_location)
            .ok()
            .flatten()
            .is_some();

        if has_const_value && !has_location {
            let value = self.get_const_value(entry);
            return VariableStatus::CompileTimeConstant { value };
        }

        // 2. Check for extern declaration (external + no location)
        let is_external = self.has_external_attr(entry);
        if is_external && !has_location {
            return VariableStatus::ExternDeclaration;
        }

        // 3. No location attribute = likely optimized out
        if !has_location {
            return VariableStatus::OptimizedOut;
        }

        // 4. Evaluate the location expression to get the address
        self.evaluate_location_to_status(unit, entry)
    }

    /// Evaluate a location attribute and return detailed status
    fn evaluate_location_to_status(
        &self,
        unit: &Unit<Reader<'a>>,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> VariableStatus {
        let attr = match entry.attr_value(gimli::DW_AT_location).ok().flatten() {
            Some(attr) => attr,
            None => return VariableStatus::NoLocation,
        };

        match attr {
            // Expression-based location (most common for global variables)
            AttributeValue::Exprloc(expr) => self.evaluate_expr_to_status(unit, &expr),

            // Direct address value (some older DWARF or simple cases)
            AttributeValue::Addr(addr) => {
                if addr == 0 {
                    VariableStatus::AddressZero
                } else {
                    VariableStatus::Valid { address: addr }
                }
            }

            // Indexed address (DWARF 5)
            AttributeValue::DebugAddrIndex(index) => match self.dwarf.address(unit, index) {
                Ok(0) => VariableStatus::AddressZero,
                Ok(addr) => VariableStatus::Valid { address: addr },
                Err(_) => VariableStatus::NoLocation,
            },

            // Location list reference
            AttributeValue::LocationListsRef(offset) => {
                match self.evaluate_location_list(unit, offset) {
                    Some(0) => VariableStatus::AddressZero,
                    Some(addr) => VariableStatus::Valid { address: addr },
                    None => VariableStatus::LocalVariable {
                        reason: "Location list with no static entry".to_string(),
                    },
                }
            }

            // Offset into location lists (DWARF 5)
            AttributeValue::DebugLocListsIndex(index) => {
                if let Ok(offset) = self.dwarf.locations_offset(unit, index) {
                    match self.evaluate_location_list(unit, offset) {
                        Some(0) => VariableStatus::AddressZero,
                        Some(addr) => VariableStatus::Valid { address: addr },
                        None => VariableStatus::LocalVariable {
                            reason: "Location list with no static entry".to_string(),
                        },
                    }
                } else {
                    VariableStatus::NoLocation
                }
            }

            // Block containing location expression (older DWARF)
            AttributeValue::Block(block) => {
                let expr = gimli::Expression(block);
                self.evaluate_expr_to_status(unit, &expr)
            }

            // Data forms that might contain addresses directly
            AttributeValue::Udata(addr) => {
                if addr == 0 {
                    VariableStatus::AddressZero
                } else {
                    VariableStatus::Valid { address: addr }
                }
            }
            AttributeValue::Data1(addr) => {
                if addr == 0 {
                    VariableStatus::AddressZero
                } else {
                    VariableStatus::Valid {
                        address: addr as u64,
                    }
                }
            }
            AttributeValue::Data2(addr) => {
                if addr == 0 {
                    VariableStatus::AddressZero
                } else {
                    VariableStatus::Valid {
                        address: addr as u64,
                    }
                }
            }
            AttributeValue::Data4(addr) => {
                if addr == 0 {
                    VariableStatus::AddressZero
                } else {
                    VariableStatus::Valid {
                        address: addr as u64,
                    }
                }
            }
            AttributeValue::Data8(addr) => {
                if addr == 0 {
                    VariableStatus::AddressZero
                } else {
                    VariableStatus::Valid { address: addr }
                }
            }
            AttributeValue::Sdata(addr) => {
                if addr == 0 {
                    VariableStatus::AddressZero
                } else {
                    VariableStatus::Valid {
                        address: addr as u64,
                    }
                }
            }

            _ => VariableStatus::NoLocation,
        }
    }

    /// Evaluate a DWARF expression and return detailed status
    fn evaluate_expr_to_status(
        &self,
        unit: &Unit<Reader<'a>>,
        expr: &gimli::Expression<Reader<'a>>,
    ) -> VariableStatus {
        let mut evaluation = (*expr).evaluation(unit.encoding());

        let mut result = match evaluation.evaluate() {
            Ok(r) => r,
            Err(_) => return VariableStatus::NoLocation,
        };

        loop {
            match result {
                // Evaluation complete - extract the address from the result
                gimli::EvaluationResult::Complete => {
                    let pieces = evaluation.result();

                    // Empty result = optimized out
                    if pieces.is_empty() {
                        return VariableStatus::OptimizedOut;
                    }

                    // Multi-piece: variable split across locations (DW_OP_piece/DW_OP_bit_piece)
                    if pieces.len() > 1 {
                        let has_address = pieces
                            .iter()
                            .any(|p| matches!(p.location, gimli::Location::Address { .. }));
                        return VariableStatus::MultiPiece { has_address };
                    }

                    // Single piece: check the location type
                    return match pieces[0].location {
                        gimli::Location::Address { address } => {
                            if address == 0 {
                                VariableStatus::AddressZero
                            } else {
                                VariableStatus::Valid { address }
                            }
                        }
                        gimli::Location::Register { register } => VariableStatus::RegisterOnly {
                            register: register.0,
                        },
                        gimli::Location::Value { .. } => VariableStatus::ImplicitValue,
                        gimli::Location::ImplicitPointer { .. } => VariableStatus::ImplicitPointer,
                        gimli::Location::Empty => VariableStatus::OptimizedOut,
                        // Bytes is used for raw data in some DWARF versions
                        gimli::Location::Bytes { .. } => VariableStatus::ImplicitValue,
                    };
                }

                // Needs an address from .debug_addr section
                gimli::EvaluationResult::RequiresIndexedAddress { index, relocate: _ } => {
                    match self.dwarf.address(unit, index) {
                        Ok(address) => match evaluation.resume_with_indexed_address(address) {
                            Ok(r) => result = r,
                            Err(_) => return VariableStatus::NoLocation,
                        },
                        Err(_) => return VariableStatus::NoLocation,
                    }
                }

                // Needs address relocation
                gimli::EvaluationResult::RequiresRelocatedAddress(address) => {
                    match evaluation.resume_with_relocated_address(address) {
                        Ok(r) => result = r,
                        Err(_) => return VariableStatus::NoLocation,
                    }
                }

                // Needs base type information
                gimli::EvaluationResult::RequiresBaseType(_) => {
                    let value_type = gimli::ValueType::Generic;
                    match evaluation.resume_with_base_type(value_type) {
                        Ok(r) => result = r,
                        Err(_) => return VariableStatus::NoLocation,
                    }
                }

                // Runtime requirements - these indicate local variables
                gimli::EvaluationResult::RequiresRegister { register, .. } => {
                    return VariableStatus::LocalVariable {
                        reason: format!("Requires register {}", register.0),
                    };
                }
                gimli::EvaluationResult::RequiresFrameBase => {
                    return VariableStatus::LocalVariable {
                        reason: "Stack variable (requires frame base)".to_string(),
                    };
                }
                gimli::EvaluationResult::RequiresMemory { .. } => {
                    return VariableStatus::LocalVariable {
                        reason: "Requires memory read at runtime".to_string(),
                    };
                }
                gimli::EvaluationResult::RequiresTls(_) => {
                    return VariableStatus::LocalVariable {
                        reason: "Thread-local storage variable".to_string(),
                    };
                }
                gimli::EvaluationResult::RequiresCallFrameCfa => {
                    return VariableStatus::LocalVariable {
                        reason: "Requires call frame CFA".to_string(),
                    };
                }
                gimli::EvaluationResult::RequiresAtLocation(_) => {
                    return VariableStatus::LocalVariable {
                        reason: "Requires DW_AT_location from another DIE".to_string(),
                    };
                }
                gimli::EvaluationResult::RequiresEntryValue(_) => {
                    return VariableStatus::LocalVariable {
                        reason: "Requires entry value (function parameter)".to_string(),
                    };
                }
                gimli::EvaluationResult::RequiresParameterRef(_) => {
                    return VariableStatus::LocalVariable {
                        reason: "Requires parameter reference".to_string(),
                    };
                }
            }
        }
    }

    /// Evaluate a location list to find the address for a global/static variable.
    /// For global variables, we look for an entry that covers "any" address or
    /// the first valid location expression.
    fn evaluate_location_list(
        &self,
        unit: &Unit<Reader<'a>>,
        offset: gimli::LocationListsOffset<<Reader<'a> as gimli::Reader>::Offset>,
    ) -> Option<u64> {
        let mut locations = self.dwarf.locations(unit, offset).ok()?;

        // Iterate through location list entries
        while let Ok(Some(entry)) = locations.next() {
            // Try to evaluate this location expression
            if let Some(addr) = self.evaluate_simple_location_expr(unit, &entry.data) {
                return Some(addr);
            }
        }
        None
    }

    /// Evaluate a DWARF location expression to extract a static address.
    ///
    /// Uses gimli's Evaluation API for proper DWARF expression handling.
    /// This correctly handles all DWARF operations including:
    /// - DW_OP_addr: Direct address (GCC, Clang)
    /// - DW_OP_addrx: Indexed address in .debug_addr (DWARF 5, GCC 11+, Clang 14+)
    /// - DW_OP_GNU_addr_index: GNU extension for indexed addresses
    /// - DW_OP_constN + DW_OP_plus_uconst: Computed addresses (some ARM compilers)
    /// - All stack-based arithmetic and logical operations
    ///
    /// Note: We only handle static/global addresses. When the evaluator requests
    /// runtime information (registers, memory, frame base, etc.), we return None
    /// as these are for local variables that can't be resolved statically.
    fn evaluate_simple_location_expr(
        &self,
        unit: &Unit<Reader<'a>>,
        expr: &gimli::Expression<Reader<'a>>,
    ) -> Option<u64> {
        let mut evaluation = (*expr).evaluation(unit.encoding());

        // Run the evaluation loop
        let mut result = evaluation.evaluate().ok()?;

        loop {
            match result {
                // Evaluation complete - extract the address from the result
                gimli::EvaluationResult::Complete => {
                    let pieces = evaluation.result();
                    // We expect a single piece with an address for global variables
                    if pieces.len() == 1 {
                        // Value on stack without Location means the address is the value itself
                        if let gimli::Location::Address { address } = pieces[0].location {
                            return Some(address);
                        }
                    }
                    // For multi-piece or non-address locations, try to get address from first piece
                    for piece in pieces {
                        if let gimli::Location::Address { address } = piece.location {
                            return Some(address);
                        }
                    }
                    return None;
                }

                // Needs an address from .debug_addr section (DW_OP_addrx, DW_OP_GNU_addr_index)
                gimli::EvaluationResult::RequiresIndexedAddress { index, relocate: _ } => {
                    let address = self.dwarf.address(unit, index).ok()?;
                    result = evaluation.resume_with_indexed_address(address).ok()?;
                }

                // Needs address relocation (for position-independent code)
                gimli::EvaluationResult::RequiresRelocatedAddress(address) => {
                    // For our purposes, we don't need to relocate - the address is already correct
                    result = evaluation.resume_with_relocated_address(address).ok()?;
                }

                // Needs base type information for typed operations
                gimli::EvaluationResult::RequiresBaseType(offset) => {
                    // For address computation, we can use a generic 64-bit unsigned type
                    let value_type = gimli::ValueType::Generic;
                    result = evaluation.resume_with_base_type(value_type).ok()?;
                    let _ = offset; // Silence unused warning
                }

                // Runtime requirements - these indicate local variables, not static addresses
                // We can't evaluate these without runtime context
                gimli::EvaluationResult::RequiresMemory { .. }
                | gimli::EvaluationResult::RequiresRegister { .. }
                | gimli::EvaluationResult::RequiresFrameBase
                | gimli::EvaluationResult::RequiresTls(_)
                | gimli::EvaluationResult::RequiresCallFrameCfa
                | gimli::EvaluationResult::RequiresAtLocation(_)
                | gimli::EvaluationResult::RequiresEntryValue(_)
                | gimli::EvaluationResult::RequiresParameterRef(_) => {
                    // These are for local variables - we can't resolve them statically
                    return None;
                }
            }
        }
    }

    fn get_virtuality(
        &self,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<gimli::DwVirtuality> {
        match entry.attr_value(gimli::DW_AT_virtuality).ok()?? {
            AttributeValue::Virtuality(v) => Some(v),
            _ => None,
        }
    }

    fn has_declaration_attr(&self, entry: &DebuggingInformationEntry<Reader<'a>>) -> bool {
        matches!(
            entry.attr_value(gimli::DW_AT_declaration).ok(),
            Some(Some(AttributeValue::Flag(true)))
        )
    }

    fn has_external_attr(&self, entry: &DebuggingInformationEntry<Reader<'a>>) -> bool {
        matches!(
            entry.attr_value(gimli::DW_AT_external).ok(),
            Some(Some(AttributeValue::Flag(true)))
        )
    }

    fn has_enum_class_attr(&self, entry: &DebuggingInformationEntry<Reader<'a>>) -> bool {
        matches!(
            entry.attr_value(gimli::DW_AT_enum_class).ok(),
            Some(Some(AttributeValue::Flag(true)))
        )
    }

    fn has_artificial_attr(&self, entry: &DebuggingInformationEntry<Reader<'a>>) -> bool {
        matches!(
            entry.attr_value(gimli::DW_AT_artificial).ok(),
            Some(Some(AttributeValue::Flag(true)))
        )
    }

    fn get_array_count(
        &self,
        unit: &Unit<Reader<'a>>,
        entry: &DebuggingInformationEntry<Reader<'a>>,
    ) -> Option<u64> {
        // Look for DW_TAG_subrange_type child with count or upper_bound
        if let Ok(mut tree) = unit.entries_tree(Some(entry.offset())) {
            if let Ok(root) = tree.root() {
                let mut children = root.children();
                while let Ok(Some(child)) = children.next() {
                    if child.entry().tag() == gimli::DW_TAG_subrange_type {
                        // Try DW_AT_count first
                        if let Ok(Some(attr)) = child.entry().attr_value(gimli::DW_AT_count) {
                            return match attr {
                                AttributeValue::Udata(v) => Some(v),
                                AttributeValue::Data1(v) => Some(v as u64),
                                AttributeValue::Data2(v) => Some(v as u64),
                                AttributeValue::Data4(v) => Some(v as u64),
                                AttributeValue::Data8(v) => Some(v),
                                _ => None,
                            };
                        }
                        // Try DW_AT_upper_bound (count = upper_bound + 1)
                        if let Ok(Some(attr)) = child.entry().attr_value(gimli::DW_AT_upper_bound) {
                            return match attr {
                                AttributeValue::Udata(v) => Some(v + 1),
                                AttributeValue::Data1(v) => Some(v as u64 + 1),
                                AttributeValue::Data2(v) => Some(v as u64 + 1),
                                AttributeValue::Data4(v) => Some(v as u64 + 1),
                                AttributeValue::Data8(v) => Some(v + 1),
                                AttributeValue::Sdata(v) => Some((v + 1) as u64),
                                _ => None,
                            };
                        }
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test ELF fixtures are embedded at compile time
    const TEST_ARM_ELF: &[u8] = include_bytes!("../tests/fixtures/test_arm.elf");
    const TEST_STRUCT_ELF: &[u8] = include_bytes!("../tests/fixtures/test_struct.elf");
    const TEST_POINTER_ELF: &[u8] = include_bytes!("../tests/fixtures/test_pointer.elf");
    const TEST_COMPLEX_C_ELF: &[u8] = include_bytes!("../tests/fixtures/test_complex_c.elf");
    const TEST_CPP_ELF: &[u8] = include_bytes!("../tests/fixtures/test_cpp.elf");

    #[test]
    fn test_dwarf_type_key() {
        let key1 = DwarfTypeKey::new(0, 100);
        let key2 = DwarfTypeKey::new(0, 100);
        let key3 = DwarfTypeKey::new(1, 100);

        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_parse_simple_variables() {
        let result = DwarfParser::parse_bytes(TEST_ARM_ELF).expect("Failed to parse ELF");

        // Check we found some variables
        assert!(!result.symbols.is_empty(), "Should find variables");

        // Look for specific variables we know are in the test ELF
        let global_counter = result.symbols.iter().find(|s| s.name == "global_counter");
        assert!(
            global_counter.is_some(),
            "Should find global_counter variable"
        );

        if let Some(var) = global_counter {
            assert!(
                var.status.is_readable(),
                "global_counter should be readable"
            );
            assert!(var.address > 0, "global_counter should have valid address");
        }

        let sensor_data = result.symbols.iter().find(|s| s.name == "sensor_data");
        assert!(sensor_data.is_some(), "Should find sensor_data variable");
    }

    #[test]
    fn test_parse_enum_types() {
        let result = DwarfParser::parse_bytes(TEST_ARM_ELF).expect("Failed to parse ELF");

        // Look for the enum variable
        let current_state = result.symbols.iter().find(|s| s.name == "current_state");
        assert!(
            current_state.is_some(),
            "Should find current_state enum variable"
        );

        // Check that we parsed enum types
        let stats = result.type_table.stats();
        assert!(stats.enums > 0, "Should have parsed enum types");
    }

    #[test]
    fn test_parse_struct_types() {
        let result = DwarfParser::parse_bytes(TEST_STRUCT_ELF).expect("Failed to parse ELF");

        // Look for struct variables
        let sensor_struct = result.symbols.iter().find(|s| s.name == "sensor_struct");
        assert!(
            sensor_struct.is_some(),
            "Should find sensor_struct variable"
        );

        // Check that we parsed struct types
        let stats = result.type_table.stats();
        assert!(stats.structs > 0, "Should have parsed struct types");

        // Check that structs have members
        if let Some(var) = sensor_struct {
            let members = result.type_table.get_members(var.type_id);
            assert!(members.is_some(), "Struct should have members");
            if let Some(members) = members {
                assert!(
                    members.len() >= 3,
                    "SensorData should have at least 3 fields (x, y, value)"
                );
            }
        }
    }

    #[test]
    fn test_parse_nested_structs() {
        let result = DwarfParser::parse_bytes(TEST_STRUCT_ELF).expect("Failed to parse ELF");

        // Look for device_config which contains a nested SensorData
        let device_config = result.symbols.iter().find(|s| s.name == "device_config");

        if let Some(var) = device_config {
            let members = result.type_table.get_members(var.type_id);
            assert!(members.is_some(), "DeviceConfig should have members");
            if let Some(members) = members {
                // Should have id, sensor (nested struct), enabled
                assert!(
                    members.len() >= 3,
                    "DeviceConfig should have at least 3 fields"
                );
            }
        }
    }

    #[test]
    fn test_parse_array_types() {
        let result = DwarfParser::parse_bytes(TEST_STRUCT_ELF).expect("Failed to parse ELF");

        // Look for the buffer array
        let buffer = result.symbols.iter().find(|s| s.name == "buffer");
        assert!(buffer.is_some(), "Should find buffer array");

        // Check array type parsing
        let stats = result.type_table.stats();
        assert!(stats.arrays > 0, "Should have parsed array types");
    }

    #[test]
    fn test_parse_pointer_types() {
        let result = DwarfParser::parse_bytes(TEST_POINTER_ELF).expect("Failed to parse ELF");

        // Look for pointer variables
        let data_ptr = result.symbols.iter().find(|s| s.name == "data_ptr");
        assert!(data_ptr.is_some(), "Should find data_ptr variable");

        let float_ptr = result.symbols.iter().find(|s| s.name == "float_ptr");
        assert!(float_ptr.is_some(), "Should find float_ptr variable");

        let double_ptr = result.symbols.iter().find(|s| s.name == "double_ptr");
        assert!(double_ptr.is_some(), "Should find double_ptr variable");

        // Check pointer type parsing
        let stats = result.type_table.stats();
        assert!(stats.pointers > 0, "Should have parsed pointer types");
    }

    #[test]
    fn test_parse_null_pointer() {
        let result = DwarfParser::parse_bytes(TEST_POINTER_ELF).expect("Failed to parse ELF");

        // Look for null_ptr
        let null_ptr = result.symbols.iter().find(|s| s.name == "null_ptr");
        assert!(null_ptr.is_some(), "Should find null_ptr variable");
    }

    #[test]
    fn test_parse_complex_c_types() {
        let result = DwarfParser::parse_bytes(TEST_COMPLEX_C_ELF).expect("Failed to parse ELF");

        // Test packed struct
        let packed_data = result.symbols.iter().find(|s| s.name == "packed_data");
        assert!(packed_data.is_some(), "Should find packed_data");

        // Test bitfield struct
        let bitfield_data = result.symbols.iter().find(|s| s.name == "bitfield_data");
        assert!(bitfield_data.is_some(), "Should find bitfield_data");

        // Test union
        let data_union = result.symbols.iter().find(|s| s.name == "data_union");
        assert!(data_union.is_some(), "Should find data_union");

        // Check union type parsing
        let stats = result.type_table.stats();
        assert!(stats.unions > 0, "Should have parsed union types");
    }

    #[test]
    fn test_parse_nested_structs_complex() {
        let result = DwarfParser::parse_bytes(TEST_COMPLEX_C_ELF).expect("Failed to parse ELF");

        // Test deeply nested struct
        let nested = result.symbols.iter().find(|s| s.name == "nested");
        assert!(nested.is_some(), "Should find nested struct variable");
    }

    #[test]
    fn test_parse_cpp_namespaces() {
        let result = DwarfParser::parse_bytes(TEST_CPP_ELF).expect("Failed to parse ELF");

        // Look for variables in namespaces
        // C++ name mangling means we might not find them by simple name
        // but they should be in the symbols list
        assert!(!result.symbols.is_empty(), "Should find C++ variables");

        // Check that temp_sensor exists (may be mangled)
        let temp_sensor = result
            .symbols
            .iter()
            .find(|s| s.name.contains("temp_sensor") || s.name == "temp_sensor");
        assert!(temp_sensor.is_some(), "Should find temp_sensor variable");
    }

    #[test]
    fn test_parse_cpp_classes() {
        let result = DwarfParser::parse_bytes(TEST_CPP_ELF).expect("Failed to parse ELF");

        // Check that we parsed class/struct types
        let stats = result.type_table.stats();
        assert!(stats.structs > 0, "Should have parsed C++ class types");
    }

    #[test]
    fn test_parse_cpp_templates() {
        let result = DwarfParser::parse_bytes(TEST_CPP_ELF).expect("Failed to parse ELF");

        // Look for template instantiations (int_container, float_container)
        let int_container = result
            .symbols
            .iter()
            .find(|s| s.name.contains("int_container"));
        assert!(
            int_container.is_some(),
            "Should find int_container template instantiation"
        );

        let float_container = result
            .symbols
            .iter()
            .find(|s| s.name.contains("float_container"));
        assert!(
            float_container.is_some(),
            "Should find float_container template instantiation"
        );
    }

    #[test]
    fn test_variable_status_classification() {
        let result = DwarfParser::parse_bytes(TEST_ARM_ELF).expect("Failed to parse ELF");

        // Count variables by status
        let readable_count = result
            .symbols
            .iter()
            .filter(|s| s.status.is_readable())
            .count();

        assert!(
            readable_count > 0,
            "Should have at least some readable variables"
        );

        // Check diagnostics
        assert!(
            result.diagnostics.total_variables > 0,
            "Should have found variables"
        );
        assert!(
            result.diagnostics.with_valid_address > 0,
            "Should have variables with valid addresses"
        );
    }

    #[test]
    fn test_type_table_completeness() {
        let result = DwarfParser::parse_bytes(TEST_STRUCT_ELF).expect("Failed to parse ELF");

        let stats = result.type_table.stats();

        // We should have parsed various type categories
        assert!(stats.primitives > 0, "Should have primitive types");
        assert!(stats.total_types > 0, "Should have total types");

        // Type table should not be empty
        assert!(
            !result.type_table.is_empty(),
            "Type table should not be empty"
        );
    }

    #[test]
    fn test_variable_addresses_are_valid() {
        let result = DwarfParser::parse_bytes(TEST_ARM_ELF).expect("Failed to parse ELF");

        for symbol in &result.symbols {
            if symbol.status.is_readable() {
                if let Some(addr) = symbol.status.address() {
                    // On ARM Cortex-M, RAM typically starts at 0x20000000
                    // Flash at 0x08000000
                    assert!(
                        addr >= 0x08000000 || addr >= 0x20000000 || addr == 0,
                        "Address should be in valid memory range: {:x}",
                        addr
                    );
                }
            }
        }
    }

    #[test]
    fn test_name_to_type_mapping() {
        let result = DwarfParser::parse_bytes(TEST_ARM_ELF).expect("Failed to parse ELF");

        // name_to_type should have entries
        assert!(
            !result.name_to_type.is_empty(),
            "Should have name-to-type mappings"
        );

        // Each symbol should have a type mapping
        for symbol in &result.symbols {
            assert!(
                result.name_to_type.contains_key(&symbol.name)
                    || symbol
                        .mangled_name
                        .as_ref()
                        .is_some_and(|n| result.name_to_type.contains_key(n)),
                "Symbol {} should have type mapping",
                symbol.name
            );
        }
    }

    #[test]
    fn test_diagnostics_accuracy() {
        let result = DwarfParser::parse_bytes(TEST_ARM_ELF).expect("Failed to parse ELF");

        let diag = &result.diagnostics;

        // Total should equal sum of categories (approximately)
        let categorized = diag.with_valid_address
            + diag.optimized_out
            + diag.local_variables
            + diag.extern_declarations
            + diag.compile_time_constants
            + diag.no_location
            + diag.unresolved_types
            + diag.address_zero
            + diag.register_only
            + diag.implicit_value
            + diag.implicit_pointer
            + diag.multi_piece
            + diag.artificial;

        // Should be close (may have some uncategorized)
        assert!(
            categorized <= diag.total_variables,
            "Categorized count should not exceed total"
        );
        assert!(
            diag.total_variables > 0,
            "Should have processed some variables"
        );
    }

    #[test]
    fn test_variable_status_methods() {
        // Test VariableStatus utility methods
        let valid_status = VariableStatus::Valid {
            address: 0x20000000,
        };
        assert!(valid_status.is_readable());
        assert_eq!(valid_status.address(), Some(0x20000000));
        assert_eq!(valid_status.reason(), "Valid address");

        let optimized_status = VariableStatus::OptimizedOut;
        assert!(!optimized_status.is_readable());
        assert_eq!(optimized_status.address(), None);
        assert_eq!(optimized_status.reason(), "Optimized out by compiler");

        let zero_status = VariableStatus::AddressZero;
        assert!(zero_status.is_readable());
        assert_eq!(zero_status.address(), Some(0));
    }

    #[test]
    fn test_parse_all_fixtures_without_panic() {
        // Ensure all test fixtures can be parsed without panicking
        let fixtures = [
            ("test_arm.elf", TEST_ARM_ELF),
            ("test_struct.elf", TEST_STRUCT_ELF),
            ("test_pointer.elf", TEST_POINTER_ELF),
            ("test_complex_c.elf", TEST_COMPLEX_C_ELF),
            ("test_cpp.elf", TEST_CPP_ELF),
        ];

        for (name, data) in &fixtures {
            let result = DwarfParser::parse_bytes(data);
            assert!(
                result.is_ok(),
                "Failed to parse {}: {:?}",
                name,
                result.err()
            );
        }
    }
}
