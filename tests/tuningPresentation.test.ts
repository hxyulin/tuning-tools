import { test } from "node:test";
import assert from "node:assert/strict";
import { sliderRange, tuningEntry, tuningNode, saveUnsupported, SAVE_UNSUPPORTED } from "../src/live/tuningPresentation.ts";
import type { CatalogEntry, SymbolNode } from "../src/elf/api.ts";

const entry: CatalogEntry = {
  id: 1, name: "led.blink_hz", unit: "Hz", kind: "f32", access: "live", default: 1,
  min: 0, max: 10, maxStep: 10, requestedAddress: 0x2020, appliedAddress: 0x2024,
};
const catalog = { address: 0x8000, symbol: "TABLE", entries: [entry] };
const node = {
  ref: { symbol: "demo::BLINK_HZ", steps: [] }, label: "BLINK_HZ", path: "demo::BLINK_HZ",
  address: 0x2000, size: 48, typeName: "Entry", wrapper: "Tunable", kind: "struct",
  scalar: null, expandable: true, childCount: 12, readable: true,
} as SymbolNode;

test("catalog-backed Entries become typed applied-value rows without losing their identity", () => {
  assert.equal(tuningEntry(node, catalog), entry);
  const shown = tuningNode(node, catalog);
  assert.equal(shown.label, "led.blink_hz");
  assert.equal(shown.scalar, "f32");
  assert.equal(shown.expandable, false);
  assert.equal(shown.ref, node.ref);
  assert.equal(shown.path, node.path);
  assert.equal(tuningEntry(shown, catalog), entry);
  assert.equal(node.expandable, true); // Raw view is unchanged.
});

test("matching is address- and type-based, with no guessing from BLINK_HZ names", () => {
  assert.equal(tuningEntry({ ...node, typeName: "Other", wrapper: null }, catalog), undefined);
  assert.equal(tuningEntry({ ...node, address: 0x3000 }, catalog), undefined);
  assert.equal(tuningEntry({ ...node, size: 4 }, catalog), undefined);
  assert.equal(tuningEntry(node, { ...catalog, entries: [entry, { ...entry, id: 2 }] }), undefined);
  assert.equal(tuningEntry(node, { ...catalog, entries: [{ ...entry, requestedAddress: 0, appliedAddress: 0 }] }), undefined);
  assert.equal(tuningEntry({ ...node, typeName: "tuning_studio_api::WatchBool", wrapper: null }, catalog), entry);
});

test("sliders ignore maxStep and preserve tiny float and integer ranges", () => {
  assert.deepEqual(sliderRange(entry), { min: 0, max: 10, step: "any" });
  assert.deepEqual(sliderRange({ ...entry, max: 1e-7, maxStep: 1e-12 }), { min: 0, max: 1e-7, step: "any" });
  assert.deepEqual(sliderRange({ ...entry, kind: "i32", min: -2.5, max: 2.5 }), { min: -2, max: 2, step: 1 });
  for (const change of [{ kind: "bool" }, { access: "readOnly" }, { max: null }, { max: Infinity }, { min: 10 }, { min: 20 }]) {
    assert.equal(sliderRange({ ...entry, ...change } as CatalogEntry), null);
  }
});

test("only explicit unsupported replies disable saving", () => {
  assert.equal(saveUnsupported(new Error(SAVE_UNSUPPORTED)), true);
  assert.equal(saveUnsupported("Saving failed: firmware storage is unavailable or the write failed."), false);
  assert.equal(saveUnsupported("the firmware did not answer in time"), false);
});
