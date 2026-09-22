import { invoke } from "@tauri-apps/api/core";

export type Step =
  | { kind: "sliceIndex"; value: number }
  | { kind: "deref" }
  | { kind: "member"; value: string }
  | { kind: "index"; value: number }
  | { kind: "variant"; value: string }
  | { kind: "discriminant" }
  /** Slot of an embassy task pool, as the task's `TaskStorage` */
  | { kind: "task"; value: number };

export interface NodeRef {
  symbol: string;
  steps: Step[];
}

export type NodeKind =
  | "scalar"
  | "enum"
  | "taggedEnum"
  | "struct"
  | "union"
  | "array"
  | "pointer"
  | "function"
  | "other";

export type Scalar =
  | "u8" | "u16" | "u32" | "u64"
  | "i8" | "i16" | "i32" | "i64"
  | "f32" | "f64" | "bool"
  | { raw: number };

export interface SymbolNode {
  ref: NodeRef;
  label: string;
  path: string;
  address: number;
  size: number | null;
  typeName: string;
  /** Outermost type when the node shows through wrappers, e.g. `Atomic<u32>` */
  wrapper: string | null;
  kind: NodeKind;
  scalar: Scalar | null;
  sequence?: boolean;
  expandable: boolean;
  childCount: number | null;
  readable: boolean;
  status: string | null;
  bitOffset: number | null;
  bitSize: number | null;
  discrValue: number | null;
  /** Where a variant is declared; an `async fn` suspend state's is its `.await` */
  location: SourceLocation | null;
}

export interface SourceLocation {
  file: string;
  line: number;
}

/** `main.rs:48` */
export function shortLocation(at: SourceLocation): string {
  return `${at.file.split(/[\\/]/).pop()}:${at.line}`;
}

export interface RootNode extends SymbolNode {
  segments: string[];
  section: string;
  readOnly: boolean;
  /** Runtime plumbing: task pools, RTT buffers, embassy and defmt state */
  internal: boolean;
}

/** One slot of an embassy task pool */
export interface Task {
  /** Label is the task name, children are the `TaskStorage` members */
  root: RootNode;
  name: string;
  slot: number;
  slots: number;
  /** Bytes of the `async fn`'s future: its RAM, as embassy tasks share one stack */
  futureSize: number | null;
  /** The states its future can be in, in declaration order */
  states: TaskPoint[];
}

/** One state of an `async fn`'s future */
export interface TaskPoint {
  /** `Unresumed`, `Returned`, `Panicked`, or `Suspend0`, `Suspend1`, … */
  label: string;
  /** The future's variant node path */
  path: string;
  /** The variant node; its children are the state's locals */
  ref: NodeRef;
  /** The `.await` a suspended task is parked on */
  location: SourceLocation | null;
}

export interface Children {
  nodes: SymbolNode[];
  total: number;
}

export interface ElfSummary {
  path: string;
  machine: string;
  is64bit: boolean;
  littleEndian: boolean;
  entryPoint: number;
  variables: number;
  functions: number;
  types: number;
  diagnostics: {
    totalVariables: number;
    withValidAddress: number;
    optimizedOut: number;
    localVariables: number;
    externDeclarations: number;
    compileTimeConstants: number;
    registerOnly: number;
  };
}

export type CellKind = "f32" | "i32" | "u32" | "bool";

export interface CatalogEntry {
  id: number;
  name: string;
  unit: string;
  kind: CellKind;
  access: "readOnly" | "live" | "safeOnly";
  default: number;
  min: number | null;
  max: number | null;
  maxStep: number | null;
  requestedAddress: number;
  appliedAddress: number;
}

export interface Catalog {
  address: number;
  symbol: string;
  entries: CatalogEntry[];
}

export interface OpenedElf {
  summary: ElfSummary;
  roots: RootNode[];
  /** embassy task slots */
  tasks: Task[];
  parseMs: number;
  /** The firmware's tuning table, when it declares one */
  catalog: Catalog | null;
  catalogError: string | null;
}

export function openElf(path: string): Promise<OpenedElf> {
  return invoke("open_elf", { path });
}

export function symbolChildren(node: NodeRef, limit?: number): Promise<Children> {
  return invoke("symbol_children", { node, limit });
}

/**
 * The node a symbol path names, e.g. `chassis::CHASSIS.wheels[0].speed`: the longest root that
 * prefixes it, then members and indexes. Null when no root matches.
 */
export function refForPath(roots: RootNode[], path: string): NodeRef | null {
  const root = roots
    .filter((r) => path === r.path || path.startsWith(`${r.path}.`) || path.startsWith(`${r.path}[`))
    .sort((a, b) => b.path.length - a.path.length)[0];
  if (!root) return null;
  const rest = path.slice(root.path.length);
  const steps: Step[] = [];
  const part = /\.([^.[\]]+)|\[(\d+)\]/y;
  while (part.lastIndex < rest.length) {
    const m = part.exec(rest);
    if (!m) return null;
    steps.push(m[1] !== undefined ? { kind: "member", value: m[1] } : { kind: "index", value: Number(m[2]) });
  }
  return { symbol: root.ref.symbol, steps: [...root.ref.steps, ...steps] };
}

export function hex(n: number): string {
  return "0x" + n.toString(16).padStart(8, "0");
}

export function scalarName(s: Scalar): string {
  return typeof s === "string" ? s : `${s.raw} raw bytes`;
}

export function startupElfPath(): Promise<string | null> {
  return invoke("startup_elf_path");
}

export function inspectChildren(node: NodeRef, offset: number, limit: number, live: boolean): Promise<Children> { return invoke("inspect_children", {node, offset, limit, live}); }
export function nodeMetadata(node: NodeRef): Promise<SymbolNode> { return invoke("node_metadata", {node}); }
