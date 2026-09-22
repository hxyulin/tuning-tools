// The simulated robot's firmware as the ELF reader would describe it: typed statics, embassy
// tasks, and the rm-telemetry tuning table.

import type { NodeKind, NodeRef, OpenedElf, RootNode, Scalar, SymbolNode, Task, TaskPoint } from "../../elf/api";
import { catalogEntries, signals, tasks, tunables } from "./firmware";

export const MOCK_ELF_PATH = "target/thumbv7em-none-eabihf/release/balance-infantry-chassis";

interface Spec {
  name: string;
  type: string;
  size: number;
  kind?: NodeKind;
  scalar?: Scalar;
  children?: Spec[];
}

const f32 = (name: string): Spec => ({ name, type: "f32", size: 4, kind: "scalar", scalar: "f32" });

const wheel = (i: number): Spec => ({
  name: `[${i}]`,
  type: "Wheel",
  size: 16,
  children: [f32("speed"), f32("target"), { name: "current", type: "i16", size: 2, kind: "scalar", scalar: "i16" }],
});

const statics: (Spec & { section: string; readOnly?: boolean })[] = [
  {
    name: "chassis::CHASSIS",
    type: "Chassis",
    size: 72,
    section: ".bss",
    children: [
      { name: "mode", type: "ChassisMode", size: 1, kind: "enum", scalar: "u8" },
      { name: "wheels", type: "[Wheel; 4]", size: 64, kind: "array", children: [0, 1, 2, 3].map(wheel) },
      { name: "debug_ptr", type: "*const f32", size: 4, kind: "pointer", scalar: "u32", children: [f32("*")] },
    ],
  },
  {
    name: "gimbal::GIMBAL",
    type: "Gimbal",
    size: 48,
    section: ".bss",
    children: [
      {
        name: "yaw",
        type: "Axis",
        size: 24,
        children: [
          f32("angle"),
          f32("target"),
          { name: "pid", type: "Pid", size: 16, children: [f32("integral"), f32("output")] },
        ],
      },
    ],
  },
  {
    name: "imu::IMU",
    type: "Bmi088",
    size: 40,
    section: ".bss",
    children: [{ name: "gyro", type: "Vec3", size: 12, children: [f32("x"), f32("y"), f32("z")] }],
  },
  {
    name: "power::POWER",
    type: "PowerMeter",
    size: 8,
    section: ".bss",
    children: [f32("buffer_energy"), f32("chassis_power")],
  },
  { name: "chassis::WHEEL_RADIUS", type: "f32", size: 4, kind: "scalar", scalar: "f32", section: ".rodata", readOnly: true },
];

/** Every node by `refKey`, and each node's children */
const nodes = new Map<string, SymbolNode>();
const children = new Map<string, SymbolNode[]>();
/** Readers for task locals, by node path */
const localReaders = new Map<string, () => number>();

const refKey = (ref: NodeRef) => JSON.stringify(ref);

export function nodeAt(ref: NodeRef): SymbolNode | undefined {
  return nodes.get(refKey(ref));
}

export function childrenOf(ref: NodeRef): SymbolNode[] {
  return children.get(refKey(ref)) ?? [];
}

/** Scalar and enum leaves under a node, as the backend's `watchable_leaves` lists them */
export function leavesOf(node: SymbolNode): SymbolNode[] {
  if (!node.readable || node.kind === "pointer" || node.ref.steps.some((s) => s.kind === "deref")) return [];
  if (node.kind === "scalar" || node.kind === "enum") return typeof node.scalar === "string" ? [node] : [];
  return childrenOf(node.ref).flatMap(leavesOf);
}

export function readerFor(node: SymbolNode): (() => number) | null {
  if (node.path === "chassis::CHASSIS.debug_ptr") return () => 0x20000100;
  if (node.path === "chassis::CHASSIS.debug_ptr.*") return signals["chassis::CHASSIS.wheels[0].speed"];
  return signals[node.path] ?? localReaders.get(node.path) ?? null;
}

function makeNode(spec: Spec, path: string, ref: NodeRef, address: number): SymbolNode {
  const kind = spec.kind ?? (spec.children ? "struct" : "other");
  const node: SymbolNode = {
    ref,
    label: spec.name.split("::").pop()!,
    path,
    address,
    size: spec.size,
    typeName: spec.type,
    wrapper: null,
    kind,
    scalar: spec.scalar ?? null,
    expandable: (spec.children?.length ?? 0) > 0,
    childCount: spec.children?.length ?? null,
    readable: true,
    status: null,
    bitOffset: null,
    bitSize: null,
    discrValue: null,
    location: null,
  };
  nodes.set(refKey(ref), node);
  let offset = address;
  children.set(
    refKey(ref),
    (spec.children ?? []).map((child) => {
      const index = child.name.startsWith("[");
      const step = kind === "pointer" ? ({ kind: "deref" } as const) : index
        ? ({ kind: "index", value: Number(child.name.slice(1, -1)) } as const)
        : ({ kind: "member", value: child.name } as const);
      const made = makeNode(child, index ? `${path}${child.name}` : `${path}.${child.name}`, { ...ref, steps: [...ref.steps, step] }, offset);
      offset += child.size;
      return made;
    }),
  );
  return node;
}

function makeTask(t: (typeof tasks)[number], address: number): Task {
  const path = `${t.module}::${t.name}`;
  const pool = `${path}::POOL`;
  const ref: NodeRef = { symbol: pool, steps: [{ kind: "task", value: 0 }] };
  const root: RootNode = {
    ...makeNode({ name: path, type: "TaskStorage", size: t.futureSize + 16 }, path, ref, address),
    segments: path.split("::"),
    section: ".bss",
    readOnly: false,
    internal: true,
  };
  const point = (label: string, locals: Spec[] = [], at: number | null = null): TaskPoint => {
    const pointPath = `${path}.future.${label}`;
    const pointRef: NodeRef = { ...ref, steps: [...ref.steps, { kind: "member", value: "future" }, { kind: "variant", value: label }] };
    makeNode({ name: label, type: label, size: t.futureSize, children: locals }, pointPath, pointRef, address + 16);
    return { label, path: pointPath, ref: pointRef, location: at === null ? null : { file: t.file, line: at } };
  };
  const suspend = point(
    "Suspend0",
    t.locals.map((l) => ({ name: l.name, type: l.scalar, size: l.scalar === "u64" ? 8 : 4, kind: "scalar", scalar: l.scalar })),
    t.line,
  );
  for (const l of t.locals) localReaders.set(`${suspend.path}.${l.name}`, l.read);
  return {
    root,
    name: path,
    slot: 0,
    slots: 1,
    futureSize: t.futureSize,
    states: [point("Unresumed"), suspend, point("Returned"), point("Panicked")],
  };
}

function build(): OpenedElf {
  let address = 0x2000_1a40;
  const roots: RootNode[] = statics.map((s) => {
    const at = s.readOnly ? 0x0801_2c00 : address;
    if (!s.readOnly) address += s.size + ((8 - (s.size % 8)) % 8);
    const node = makeNode(s, s.name, { symbol: s.name, steps: [] }, at);
    return { ...node, segments: s.name.split("::"), section: s.section, readOnly: s.readOnly ?? false, internal: false };
  });
  for (const entry of catalogEntries) {
    const name = `tuning::${entry.name.replace(/\./g, "_").toUpperCase()}`;
    const spec: Spec = { name, type: "Entry", size: 8, children: [
      { name: "requested", type: "u32", size: 4, kind: "scalar", scalar: "u32" },
      { name: "applied", type: "u32", size: 4, kind: "scalar", scalar: "u32" },
    ] };
    const node = makeNode(spec, name, { symbol: name, steps: [] }, entry.requestedAddress);
    roots.push({ ...node, wrapper: "Tunable", segments: name.split("::"), section: ".data", readOnly: false, internal: false });
    const bits = (value: number) => new Uint32Array(new Float32Array([value]).buffer)[0];
    localReaders.set(`${name}.requested`, () => bits(tunables[entry.id].requested));
    localReaders.set(`${name}.applied`, () => bits(tunables[entry.id].applied));
  }
  const taskList = tasks.map((t, i) => makeTask(t, 0x2000_4000 + i * 0x800));
  return {
    summary: {
      path: MOCK_ELF_PATH,
      machine: "ARMv7E-M",
      is64bit: false,
      littleEndian: true,
      entryPoint: 0x0800_0299,
      variables: 412,
      functions: 3120,
      types: 1804,
      diagnostics: {
        totalVariables: 412,
        withValidAddress: 398,
        optimizedOut: 9,
        localVariables: 0,
        externDeclarations: 3,
        compileTimeConstants: 2,
        registerOnly: 0,
      },
    },
    roots,
    tasks: taskList,
    parseMs: 184,
    catalog: { address: 0x2000_0400, symbol: "rm_telemetry::TABLE", entries: catalogEntries },
    catalogError: null,
  };
}

export const mockElf = build();
