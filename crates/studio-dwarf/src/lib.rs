//! ELF symbol and DWARF type inspection for tuning-tools.
//!
//! Ported from `datavis-rs` (MIT, `src/backend/{elf_parser,dwarf_parser,type_table}.rs`
//! at `cb4c641`). The parser and type table are kept as they were; the ELF layer
//! drops the datavis `Variable` coupling and gains [`rebuild`] for re-resolving
//! symbols after a firmware rebuild and [`tree`] for browsing statics by path.

pub mod dwarf_parser;
pub mod elf;
pub mod error;
pub mod rebuild;
pub mod task_stats;
pub mod task_trace;
pub mod tasks;
pub mod tree;
pub mod type_table;
pub mod variable_type;

pub use dwarf_parser::{
    DwarfDiagnostics, DwarfParseResult, DwarfParser, ParsedSymbol, VariableStatus,
};
pub use elf::{demangle_symbol, ElfInfo, ElfParser, SymbolInfo, SymbolType};
pub use error::{Error, Result};
pub use rebuild::{diff_symbols, SymbolChange, TrackedSymbol};
pub use tree::{NodeRef, Step, SymbolNode};
pub use type_table::{SharedTypeTable, TypeDef, TypeHandle, TypeId, TypeTable};
pub use variable_type::VariableType;
