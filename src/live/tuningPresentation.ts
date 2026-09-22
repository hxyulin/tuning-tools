import type { Catalog, CatalogEntry, SymbolNode } from "../elf/api.ts";

/** Match descriptors by both their type and catalog cell addresses, never by a variable's name. */
export function tuningEntry(node: SymbolNode, catalog: Catalog | null): CatalogEntry | undefined {
  const types = [node.typeName, node.wrapper ?? ""];
  if (!types.some((type) => /^(?:(?:tuning_studio_api|rm_telemetry)::)?(?:Entry|Tunable|WatchF32|WatchI32|WatchU32|WatchBool)$/.test(type))) return;
  if (!node.readable || node.size === null || node.size <= 0 || node.address === 0) return;
  const end = node.address + node.size;
  const entries = catalog?.entries.filter((entry) =>
    entry.appliedAddress >= node.address && entry.appliedAddress + 4 <= end &&
    entry.requestedAddress >= node.address && entry.requestedAddress + 4 <= end,
  ) ?? [];
  return entries.length === 1 ? entries[0] : undefined;
}

export function tuningNode(node: SymbolNode, catalog: Catalog | null): SymbolNode {
  const entry = tuningEntry(node, catalog);
  return entry ? { ...node, label: entry.name, typeName: entry.kind, wrapper: node.wrapper ?? node.typeName,
    kind: "scalar", scalar: entry.kind, expandable: false, childCount: 0 } : node;
}

/** Slider resolution is independent of the firmware's per-update slew limit. */
export function sliderRange(entry: CatalogEntry): { min: number; max: number; step: number | "any" } | null {
  const { min, max } = entry;
  if (entry.access === "readOnly" || entry.kind === "bool" || min === null || max === null ||
      !Number.isFinite(min) || !Number.isFinite(max) || min >= max || !Number.isFinite(max - min)) return null;
  const integer = entry.kind !== "f32";
  const low = integer ? Math.ceil(min) : min;
  const high = integer ? Math.floor(max) : max;
  return low < high ? { min: low, max: high, step: integer ? 1 : "any" } : null;
}

export const SAVE_UNSUPPORTED = "This firmware does not support saving. Changes are temporary and reset on restart.";
export function saveUnsupported(error: unknown): boolean {
  return String(error).includes(SAVE_UNSUPPORTED);
}
