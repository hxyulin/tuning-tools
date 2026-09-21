// Run the extension headlessly against scripts/fakeVscode.ts and a `studio-server --mock`: open the
// panel, drive it as the page would, and hand the probe to a simulated probe-rs debug session.
//
//   npm run harness

import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { connect } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { harness, Uri, workspace } from "./fakeVscode";
import { activate } from "../src/extension";

const REPO = resolve(__dirname, "../..");
const FIXTURE = resolve(REPO, "crates/studio-dwarf/tests/fixtures/test_arm.elf");
const TTS1 = 0x31535454;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function until<T>(what: string, check: () => T | undefined | null | false, ms = 5000): Promise<T> {
  const end = Date.now() + ms;
  for (;;) {
    const v = check();
    if (v) return v;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await sleep(10);
  }
}

async function main() {
  const state = new Map<string, unknown>();
  const context = {
    extensionPath: resolve(__dirname, ".."),
    extensionUri: Uri.file(resolve(__dirname, "..")),
    subscriptions: [] as { dispose(): void }[],
    workspaceState: {
      get: (k: string) => state.get(k),
      update: async (k: string, v: unknown) => void state.set(k, v),
    },
  };
  harness.settings["tuningStudio.mockTarget"] = true;
  workspace.workspaceFolders = [{ uri: Uri.file(REPO), name: "tuning-tools", index: 0 }];
  harness.launch = [
    { type: "cortex-debug", name: "other" },
    { type: "probe-rs-debug", name: "Debug (attach)", chip: "STM32H723VGTx", coreConfigs: [{ programBinary: FIXTURE }] },
  ];

  activate(context as never);
  await harness.commands.get("tuningStudio.open")!();
  let panel = harness.panels[0];
  assert.ok(panel, "panel opened");
  assert.equal((panel.createOptions as { retainContextWhenHidden: boolean }).retainContextWhenHidden, true);

  // The page, with its assets rewritten and every script under the nonce
  const html: string = panel.webview.html;
  const nonce = /script-src 'nonce-([^']+)'/.exec(html)?.[1];
  assert.ok(nonce, "CSP with a script nonce");
  assert.match(html, /<meta http-equiv="Content-Security-Policy"/);
  for (const tag of html.match(/<script [^>]*src=[^>]*>/g) ?? []) assert.ok(tag.includes(`nonce="${nonce}"`), tag);
  assert.doesNotMatch(html, /(src|href)="\.\//, "no relative asset paths left");
  assert.match(html, /src="https:\/\/file\+\.vscode-resource\.test[^"]+\/media\/assets\/index-[^"]+\.js"/);
  assert.match(html, /<script id="tuning-studio-boot" type="application\/json">\{"storage":\{\}\}<\/script>/);
  console.log("page: CSP, nonce and webview URIs ok");

  let nextId = 1;
  async function call<T = any>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    const id = nextId++;
    panel.send({ type: "call", id, method, params });
    const m = await until(`answer to ${method}`, () => panel.toPage.find((m: any) => m.type === "result" && m.id === id));
    if (!m.ok) throw new Error(m.error);
    return m.result;
  }
  const lastStatus = () => [...panel.toPage].reverse().find((m: any) => m.type === "status")?.status;
  const frames = (session: number) => panel.toPage.filter((m: any) => m.type === "frame" && m.session === session);
  const statusEvents = (session: number) =>
    panel.toPage
      .filter((m: any) => m.type === "event" && m.session === session && m.event.type === "status")
      .map((m: any) => m.event.state);

  const status = await call("host_status");
  assert.equal(status.sources.probe.available, true);
  assert.equal(status.sources.debugger.available, false);

  const startup = await call("startup");
  assert.equal(startup.elfPath, FIXTURE, "ELF from the launch configuration");
  assert.equal(startup.connectDefaults.chip, "STM32H723VGTx");
  assert.equal(startup.connect, null, "never connects by itself");
  console.log("startup: launch.json chip and ELF picked up");

  const opened = await call("open_elf", { path: FIXTURE });
  assert.equal(state.get("tuningStudio.lastElf"), FIXTURE);
  const root = opened.roots.find((r: any) => r.path === "global_counter");
  const leaves = await call("watchable_leaves", { node: root.ref });
  const results = await call("session_set_watches", { watches: [{ id: 5, node: leaves[0].ref, cell: null }] });
  assert.deepEqual(results, [{ id: 5, error: null }]);

  const request = { carrier: "probe", probe: null, chip: "STM32H723VGTx", speedKhz: 4000, port: null, rateHz: 200 };
  await call("session_connect", { request, session: 42 });
  await until("frames", () => frames(42).length >= 3);
  const frame = frames(42)[0].data;
  assert.ok(frame instanceof Uint8Array && frame.byteOffset === 0 && frame.byteLength === frame.buffer.byteLength);
  assert.equal(new DataView(frame.buffer).getUint32(0, true), TTS1);
  assert.deepEqual(statusEvents(42).slice(0, 2), ["connecting", "connected"]);
  console.log(`session 42: ${frames(42).length} frames relayed as own-buffer Uint8Arrays`);

  // A probe-rs launch resolves: the probe is released before the configuration is handed back
  const provider = harness.configProviders[0];
  const config = await provider.resolveDebugConfigurationWithSubstitutedVariables!(undefined, {
    type: "probe-rs-debug",
    name: "Debug (attach)",
  });
  assert.equal(config.name, "Debug (attach)", "configuration passed through");
  await until("disconnected", () => statusEvents(42).includes("disconnected"));
  const blocked = lastStatus();
  assert.equal(blocked.sources.probe.available, false);
  assert.match(blocked.sources.probe.reason, /Debug \(attach\)/);
  assert.equal(blocked.sources.serial.available, true);
  assert.match(blocked.notice.message, /Released the probe/);
  assert.equal(blocked.notice.reconnect, false);
  assert.ok(harness.info.some((m) => m.includes("released the probe")));
  await assert.rejects(call("session_connect", { request, session: 43 }), /using the probe/);
  console.log("debug launch: probe released before attach, Probe source blocked, USB open");

  const session = { id: "dap-1", name: "Debug (attach)", type: "probe-rs-debug" };
  harness.started.fire(session);
  assert.equal(lastStatus().sources.probe.available, false);
  harness.terminated.fire(session);
  const free = lastStatus();
  assert.equal(free.sources.probe.available, true);
  assert.equal(free.notice.reconnect, true);
  assert.match(free.notice.message, /ended/);
  console.log("debug session ended: notice offers to reconnect");

  await call("session_connect", { request, session: 44 });
  await until("frames after reconnect", () => frames(44).length >= 2);
  assert.equal(lastStatus().notice, null, "connecting clears the notice");
  console.log(`session 44: reconnected, ${frames(44).length} frames`);

  // Recording goes to the workspace's .tuning-studio/recordings; a scratch workspace here
  const scratch = mkdtempSync(join(tmpdir(), "tuning-studio-harness-"));
  const folders = workspace.workspaceFolders;
  workspace.workspaceFolders = [{ uri: Uri.file(scratch), name: "scratch", index: 0 }];
  const appEvents = () => panel.toPage.filter((m: any) => m.type === "app_event").map((m: any) => m.event);
  const started = await call("recording_start", { path: null });
  assert.ok(started.path.startsWith(join(scratch, ".tuning-studio", "recordings", "test_arm-")), started.path);
  await until("recording progress", () =>
    appEvents().find((e: any) => e.type === "recording" && e.active && e.ticks > 0),
  );

  const stream = await call("stream_start", { port: 0, bindAll: false });
  assert.match(stream.address, /^127\.0\.0\.1:\d+$/);
  const [, port] = stream.address.split(":");
  const lines: any[] = [];
  const socket = connect(Number(port), "127.0.0.1");
  let buffered = "";
  socket.on("data", (chunk) => {
    buffered += chunk.toString("utf8");
    let nl: number;
    while ((nl = buffered.indexOf("\n")) >= 0) {
      lines.push(JSON.parse(buffered.slice(0, nl)));
      buffered = buffered.slice(nl + 1);
    }
  });
  await until("stream samples", () => lines.some((l) => l.type === "samples"));
  assert.equal(lines[0].type, "hello");
  assert.equal(lines[0].watches[0].name, "global_counter");
  await until("a stream client", () => appEvents().find((e: any) => e.type === "stream" && e.clients === 1));

  const stopped = await call("recording_stop");
  assert.equal(stopped.active, false);
  assert.ok(stopped.ticks > 0 && stopped.bytes > 0);
  harness.savePath = join(scratch, "out.csv");
  const csvPath = await call("pick_csv_path", { suggested: stopped.path.replace(/\.mcap$/, ".csv") });
  assert.equal(csvPath, harness.savePath);
  const csv = await call("export_csv", { mcapPath: stopped.path, csvPath });
  assert.equal(csv.rows, stopped.ticks);
  assert.equal(readFileSync(csvPath, "utf8").split("\n")[0], "time,global_counter");
  await call("reveal", { path: csvPath });
  assert.equal(harness.executed.at(-1)?.name, "revealFileInOS");
  socket.destroy();
  const off = await call("stream_stop");
  assert.equal(off.listening, false);
  console.log(
    `recording: ${stopped.ticks} ticks, ${stopped.bytes} bytes; CSV ${csv.rows} rows; ` +
      `stream: ${lines.filter((l) => l.type === "samples").length} samples messages`,
  );
  assert.ok(existsSync(stopped.path));
  rmSync(scratch, { recursive: true, force: true });
  workspace.workspaceFolders = folders;

  panel.send({ type: "storage", key: "scope", value: '{"windowSec":5}' });
  await sleep(10);
  assert.deepEqual(state.get("tuningStudio.storage"), { scope: '{"windowSec":5}' });

  // Closing a view must leave the session and recording alive.
  const starts = harness.log.filter(l => l.startsWith("starting ")).length;
  const trees = harness.trees.get("tuningStudio.symbols");
  assert.ok((await trees.getChildren()).some((n: any) => n.path === "global_counter"));
  assert.match(harness.statusBars[0].text, /connected/);
  const lifecycleScratch = mkdtempSync(join(tmpdir(), "tuning-studio-lifecycle-"));
  workspace.workspaceFolders = [{ uri: Uri.file(lifecycleScratch), name: "lifecycle", index: 0 }];
  await harness.commands.get("tuningStudio.startRecording")!();
  await until("native recording indicator", () => /record/.test(harness.statusBars[0].text));
  panel.dispose();
  await sleep(100);
  await harness.commands.get("tuningStudio.open")!();
  panel = harness.panels.at(-1);
  const resumed = await call("startup");
  assert.equal(resumed.resumeSession, 44, "scope reattaches to the same session");
  assert.deepEqual(resumed.connect, request);
  const cached = await call("open_elf", { path: FIXTURE });
  assert.equal(cached.summary.path, FIXTURE);
  const snapshot = await call("session_attach", { session: 44 });
  assert.ok(snapshot.some((e: any) => e.type === "status" && e.state === "connected"));
  await until("frames after reopening scope", () => frames(44).length >= 2);
  const liveRecording = await call("app_state");
  assert.equal(liveRecording.recording.active, true, "recording survives scope closure");
  const nativeStopped = await harness.commands.get("tuningStudio.stopRecording")!() as { ticks: number };
  assert.ok(nativeStopped.ticks > 0);
  workspace.workspaceFolders = folders;
  rmSync(lifecycleScratch, { recursive: true, force: true });
  assert.equal(harness.log.filter(l => l.startsWith("starting ")).length, starts, "server wasn't restarted");
  await assert.rejects(call("session_attach", { session: 999 }), /session ended/);
  await assert.rejects(call("open_elf", { path: "different.elf" }), /Disconnect/);
  await assert.rejects(call("session_connect", { request, session: 45 }), /Disconnect/);

  await harness.commands.get("tuningStudio.plotSymbol")!(root);
  assert.deepEqual(await call("take_pending_watches"), [root]);
  assert.deepEqual(await call("take_pending_watches"), []);
  await harness.commands.get("tuningStudio.disconnect")!();
  await until("native disconnect", () => /disconnected/.test(harness.statusBars[0].text));

  // Native connection wizard and Explorer ELF command work without a scope.
  panel.dispose();
  await harness.commands.get("tuningStudio.openElf")!(Uri.file(FIXTURE));
  harness.quickPicks.push(0, 0);
  harness.inputValues.push("STM32H723VGTx");
  await harness.commands.get("tuningStudio.connect")!();
  await until("native connection", () => /: connected/.test(harness.statusBars[0].text));
  await harness.commands.get("tuningStudio.open")!();
  panel = harness.panels.at(-1);
  const native = await call("startup");
  assert.ok(native.resumeSession);
  assert.equal(native.connect.chip, "STM32H723VGTx");
  await call("session_attach", { session: native.resumeSession });
  harness.quickPicks.push(3);
  await harness.commands.get("tuningStudio.setRate")!();
  assert.equal((await call("startup")).connect.rateHz, 200);
  await harness.commands.get("tuningStudio.showSource")!({ ...root, location: { file: "src/main.rs", line: 2 } });
  assert.equal(harness.documents.at(-1), join(REPO, "src/main.rs"));
  await harness.commands.get("tuningStudio.disconnect")!();
  await harness.commands.get("tuningStudio.reconnect")!();
  await until("native reconnect", () => /: connected/.test(harness.statusBars[0].text));
  const retried = await call("startup");
  assert.equal(retried.connect.chip, native.connect.chip);
  assert.equal(retried.connect.probe, native.connect.probe);
  assert.equal(retried.connect.rateHz, 200, "reconnect retains the latest sample rate");
  assert.notEqual(retried.resumeSession, native.resumeSession);
  await harness.commands.get("tuningStudio.disconnect")!();
  assert.equal(harness.log.filter(l => l.startsWith("starting ")).length, starts);
  panel.dispose();
  for (const subscription of context.subscriptions) subscription.dispose();
  await sleep(500);
  console.log("native workbench: trees, commands, session reattachment and shutdown ok");
  assert.deepEqual(harness.errors, [], "no error messages");
  console.log("panel closed; extension log:");
  for (const line of harness.log) console.log(`  ${line}`);
}

main().then(
  () => process.exit(0),
  (e) => {
    console.error(e);
    process.exit(1);
  },
);
