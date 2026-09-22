// Read-only regression suite against initialized ELF memory, or the flashed lab.
// Run from vscode: node scripts/inspectorLab.mjs [--hardware]
import assert from "node:assert/strict";
import { build } from "esbuild";
import { createRequire } from "node:module";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const hardware = process.argv.includes("--hardware");
const temp = mkdtempSync(resolve(tmpdir(), "inspector-lab-"));
await build({ entryPoints: [resolve(root, "vscode/src/server.ts")], bundle: true, platform: "node", format: "cjs", outfile: resolve(temp, "server.cjs") });
const { StudioServer } = createRequire(import.meta.url)(resolve(temp, "server.cjs"));
const server = new StudioServer(resolve(root, "target/debug/studio-server"), hardware ? ["--read-only"] : ["--mock", "--read-only"], { event() {}, frame() {}, exit() {}, log: console.error });
const child = (node, step) => ({ symbol: node.symbol, steps: [...node.steps, step] });
const member = (node, name) => child(node, { kind: "member", value: name });
const index = (node, value) => child(node, { kind: "index", value });
const deref = (node) => child(node, { kind: "deref" });
const symbol = (name) => ({ symbol: `inspector_lab::${name}`, steps: [] });
const children = (node, limit) => server.call("symbol_children", { node, limit });
const read = async (nodes) => server.call("session_read_values", { nodes });
try {
  await server.ready;
  const elf = await server.call("open_elf", { path: resolve(root, "crates/studio-dwarf/tests/fixtures/inspector_lab.elf") });
  assert.equal(elf.tasks.length, 6, "main, producer, consumer, finished task and two parked slots");
  assert.equal(elf.tasks.filter((t) => t.name === "parked").length, 2);
  assert(elf.tasks.find((t) => t.name === "producer").states.filter((s) => s.label.startsWith("Suspend")).length >= 2);
  const fields = (await children(symbol("ROBOT"))).nodes;
  for (const [name, kind] of [["axes", "array"], ["matrix", "array"], ["mode", "enum"], ["command", "taggedEnum"], ["optional", "taggedEnum"], ["niche", "taggedEnum"], ["result", "taggedEnum"], ["bits", "union"], ["scalars", "struct"]]) {
    assert.equal(fields.find((f) => f.label === name)?.kind, kind, name);
  }
  const array = await children(symbol("LARGE_ARRAY"));
  assert.equal(array.total, 300);
  assert.equal(array.nodes.length, 256, "default array page is bounded");
  assert.equal((await children(symbol("LARGE_ARRAY"), 300)).nodes.length, 300);
  const probes = await server.call("list_probes");
  assert(probes.length, "a probe is required");
  await server.call("session_connect", { session: 910, request: { carrier: "probe", chip: hardware ? "STM32H723VGTx" : "mock", probe: probes[0].selector, port: null, speedKhz: 4000, rateHz: 100 } });
  if (hardware) {
    let initialized = false;
    for (let i = 0; i < 20; i++) {
      const [updates] = await read([symbol("UPDATES")]);
      if (updates.value > 0) { initialized = true; break; }
      await new Promise((done) => setTimeout(done, 250));
    }
    assert(initialized, "lab must be flashed, reset and running");
  }
  const scalar = (name) => member(member(symbol("ROBOT"), "scalars"), name);
  const nodes = [
    member(index(member(symbol("ROBOT"), "axes"), 1), "velocity"),
    index(index(member(symbol("ROBOT"), "matrix"), 1), 3),
    member(scalar("unsigned"), "__3"), member(scalar("signed"), "__3"),
    scalar("enabled"), scalar("letter"), member(scalar("finite"), "__1"),
    index(scalar("special"), 0), index(scalar("special"), 1), index(scalar("special"), 2),
    member(member(symbol("ROBOT"), "bits"), "float"),
    member(member(symbol("ROBOT"), "bits"), "word"),
    member(deref(member(symbol("LINK_A"), "next")), "value"),
    member(deref(member(deref(member(symbol("LINK_A"), "next")), "next")), "value"),
    member(deref(symbol("NULL_PTR")), "mode"),
  ];
  const values = await read(nodes);
  for (let i = 0; i < values.length - 1; i++) assert.equal(values[i].error, null, JSON.stringify(nodes[i]));
  assert.equal(values[0].value, -2.5); assert.equal(values[1].value, -8);
  assert.equal(values[2].text, "18446744073709551615"); assert.equal(values[3].text, "-9223372036854775808");
  assert.equal(values[4].value, 1); assert.equal(values[5].value, 955, "Rust char occupies four bytes");
  assert.equal(values[6].value, -2.5);
  assert.equal(values[7].text, "NaN"); assert.equal(values[8].text, "Infinity"); assert.equal(values[9].text, "-0");
  assert.equal(values[10].value, 1); assert.equal(values[11].value, 0x3f800000);
  assert.equal(values[12].value, 222); assert.equal(values[13].value, 111);
  assert.match(values[14].error, /null pointer/);
  const previews = await read([symbol("TEXT"), symbol("EMPTY_TEXT"), symbol("SLICE"), scalar("letter")]);
  assert.equal(previews[0].text, '"Robot λ — ready"'); assert.equal(previews[1].text, '""');
  assert.equal(previews[2].text, "[5 elements]"); assert.match(previews[3].text, /λ/);
  const slice = await server.call("inspect_children", {node: symbol("SLICE"), offset: 3, limit: 64, live: true});
  assert.equal(slice.total, 5); assert.deepEqual(slice.nodes.map((n) => n.label), ["[3]", "[4]"]);
  assert.deepEqual((await read(slice.nodes.map((n) => n.ref))).map((v) => v.value), [40, 50]);
  const page = await server.call("inspect_children", {node: symbol("LARGE_ARRAY"), offset: 295, limit: 64, live: true});
  assert.equal(page.nodes.length, 5); assert.equal(page.nodes[4].label, "[299]");
  assert.equal((await server.call("node_metadata", {node: slice.nodes[0].ref})).scalar, "u16");
  if (!hardware) {
    const tagged = await read([
      child(member(symbol("ROBOT"), "command"), { kind: "discriminant" }),
      member(child(member(symbol("ROBOT"), "optional"), { kind: "variant", value: "Some" }), "__0"),
      child(member(symbol("ROBOT"), "niche"), { kind: "discriminant" }),
      member(child(member(symbol("ROBOT"), "result"), { kind: "variant", value: "Ok" }), "__0"),
    ]);
    assert.deepEqual(tagged.map((v) => v.value), [0, 3.5, 42, 123]);
    const enums = await read([member(symbol("ROBOT"), "mode"), member(symbol("ROBOT"), "command"), member(symbol("ROBOT"), "niche"), member(child(member(symbol("ROBOT"), "command"), {kind: "variant", value: "Position"}), "target")]);
    assert.equal(enums[0].text, "Idle (0)"); assert.equal(enums[1].activeVariant, "Stop"); assert.equal(enums[2].activeVariant, "Some"); assert.match(enums[3].error, /inactive variant/);
    const pointer = await read([member(deref(deref(symbol("LINK_SLOT"))), "value")]);
    assert.equal(pointer[0].value, 111);
  } else {
    let tracePolls = 0;
    const seen = new Set(); const states = new Set(); const updates = new Set();
    for (let i = 0; i < 18; i++) {
      const values = await read([member(deref(deref(symbol("LINK_SLOT"))), "value"), symbol("UPDATES"), index(deref(symbol("ARRAY_PTR")), 299)]);
      seen.add(values[0].error?.includes("null pointer") ? "null" : values[0].value);
      assert.equal(values[1].error, null); updates.add(values[1].value);
      assert.equal(values[2].value, 299);
      const trace = await server.call("session_task_trace");
      assert.equal(trace.clockHz, 64_000_000); assert(trace.events.length > 0);
      const begins = new Map();
      for (const e of trace.events) { if (e.kind === 2) begins.set(e.task, e.ticks); if (e.kind === 3 && begins.has(e.task)) { assert(e.ticks >= begins.get(e.task)); tracePolls++; } }
      const snapshot = await server.call("session_task_states");
      assert.equal(snapshot.statsError, null); assert.equal(snapshot.hasStats, true); assert.equal(snapshot.clockHz, 64_000_000);
      const status = (name, slot = 0) => snapshot.tasks.find((s) => s.path === elf.tasks.find((t) => t.name === name && t.slot === slot).root.path);
      assert.equal(status("parked").state.spawned, true);
      assert.equal(status("parked", 1).state.spawned, false);
      assert.equal(status("finishes").state.spawned, false);
      assert(status("consumer").counters.polls > 0);
      states.add(status("producer").state.at?.label);
      await new Promise((done) => setTimeout(done, 250));
    }
    assert(tracePolls > 0, "timestamped poll pairs captured over SWD");
    assert(seen.has(111) && seen.has(222) && seen.has("null"), `retargets observed: ${[...seen]}`);
    assert(updates.size > 1, "firmware must be running");
    assert(states.has("Suspend0") && states.has("Suspend1"), `await states: ${[...states]}`);
    console.log("hardware: changing/null/nested pointers, index 299, running/finished/unspawned tasks, both await points and CPU counters pass");
  }
  console.log("complex types: structs, tuples, nested/large arrays, enums, Option/niches, Result, unions, Unicode, u64/i64, float specials and pointer cycles pass");
} finally {
  await server.call("session_disconnect").catch(() => {});
  server.dispose();
  rmSync(temp, { recursive: true, force: true });
}
