// Drive studio-server from Node through the extension's own client (src/server.ts): open an ELF,
// watch values, sample for a while, disconnect and shut the server down.
//
//   node dist/smoke.js --mock --elf ../crates/studio-dwarf/tests/fixtures/test_arm.elf
//   node dist/smoke.js --read-only --elf <firmware> --chip STM32H723VGTx --rate 100 --seconds 4 --watch chassis
//
// --watch takes a symbol path prefix and may repeat; without it the first few readable roots are
// watched. Exits non-zero when no frames arrive or the link fails.

import { existsSync, statSync } from "node:fs";
import { resolve } from "node:path";
import { StudioServer } from "../src/server";

function arg(name: string): string | undefined {
  const at = process.argv.indexOf(`--${name}`);
  return at >= 0 ? process.argv[at + 1] : undefined;
}
const flag = (name: string) => process.argv.includes(`--${name}`);
const all = (name: string) => process.argv.flatMap((a, i) => (a === `--${name}` ? [process.argv[i + 1]] : []));

function defaultServer(): string {
  const found = ["release", "debug"]
    .map((p) => resolve(__dirname, "../../target", p, "studio-server"))
    .filter(existsSync)
    .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  if (!found.length) throw new Error("no target/{release,debug}/studio-server; run cargo build -p tuning-studio-server");
  return found[0];
}

interface Root {
  path: string;
  ref: unknown;
  readable: boolean;
  internal: boolean;
  readOnly: boolean;
  section: string;
}
interface Leaf {
  path: string;
  ref: unknown;
}

const TTS1 = 0x31535454;

async function main() {
  const serverPath = arg("server") ?? defaultServer();
  const elf = resolve(arg("elf") ?? resolve(__dirname, "../../crates/studio-dwarf/tests/fixtures/test_arm.elf"));
  const mock = flag("mock");
  const rateHz = Number(arg("rate") ?? 100);
  const seconds = Number(arg("seconds") ?? 3);
  const chip = arg("chip") ?? "STM32H723VGTx";
  const args = [...(mock ? ["--mock"] : []), ...(flag("read-only") ? ["--read-only"] : [])];

  const session = 1;
  const states: string[] = [];
  let frames = 0;
  let ticks = 0;
  let lastStats: Record<string, unknown> | null = null;
  const errors = new Set<string>();
  let logLines = 0;
  let check = "none";
  const last = new Map<number, number>();

  const server = new StudioServer(serverPath, args, {
    event(s, event) {
      if (s !== session) return;
      if (event.type === "status") {
        states.push(`${event.state}${event.message ? ` (${event.message})` : ""}`);
      } else if (event.type === "stats") {
        lastStats = event;
        if (event.lastError) errors.add(String(event.lastError));
      } else if (event.type === "tune") {
        const c = event.check as { state: string; message?: string };
        check = c.state + (c.message ? ` (${c.message})` : "");
      } else if (event.type === "log") {
        logLines += (event.lines as unknown[]).length;
      }
    },
    frame(s, tts1) {
      if (s !== session) return;
      const view = new DataView(tts1.buffer, tts1.byteOffset, tts1.byteLength);
      if (view.getUint32(0, true) !== TTS1) throw new Error("bad frame magic");
      const n = view.getUint32(4, true);
      const columns = view.getUint32(8, true);
      let at = 16 + 8 * n;
      for (let c = 0; c < columns; c++) {
        last.set(view.getUint32(at, true), view.getFloat64(at + 8 + 8 * (n - 1), true));
        at += 8 + 8 * n;
      }
      frames++;
      ticks += n;
    },
    exit(code, expected) {
      if (!expected) console.error(`studio-server exited unexpectedly (${code})`);
    },
    log: (line) => console.error(`[server] ${line}`),
  });

  const ready = await server.ready;
  console.log(`studio-server ${ready.version} (${serverPath})${ready.mock ? ", mock target" : ""}`);
  const opened = await server.call<{ roots: Root[]; parseMs: number; summary: { variables: number } }>("open_elf", {
    path: elf,
  });
  console.log(`opened ${elf}: ${opened.summary.variables} variables, ${opened.roots.length} roots, ${opened.parseMs} ms`);

  if (flag("list")) {
    for (const r of opened.roots.filter((r) => r.readable && !r.internal)) console.log(`  ${r.path} [${r.section}]`);
    await server.dispose();
    return;
  }
  const prefixes = all("watch");
  const roots = opened.roots.filter((r) => r.readable && !r.internal && !r.readOnly);
  const picked = prefixes.length
    ? roots.filter((r) => prefixes.some((p) => r.path === p || r.path.startsWith(p)))
    : roots.slice(0, 4);
  const leaves: Leaf[] = [];
  for (const root of picked) {
    const found = await server.call<Leaf[]>("watchable_leaves", { node: root.ref }).catch(() => []);
    leaves.push(...found.slice(0, 16 - leaves.length));
    if (leaves.length >= 16) break;
  }
  if (!leaves.length) throw new Error("nothing to watch");
  const results = await server.call<{ id: number; error: string | null }[]>("session_set_watches", {
    watches: leaves.map((l, i) => ({ id: i + 1, node: l.ref, cell: null })),
  });
  const bad = results.filter((r) => r.error);
  console.log(`watching ${leaves.length - bad.length} of ${leaves.length} values${bad.length ? `; ${bad[0].error}` : ""}`);

  if (!mock) {
    const probes = await server.call<{ selector: string; name: string }[]>("list_probes");
    console.log(`probes: ${probes.map((p) => `${p.name} [${p.selector}]`).join(", ") || "none"}`);
  }
  const started = Date.now();
  await server.call("session_connect", {
    request: { carrier: "probe", probe: null, chip, speedKhz: Number(arg("speed") ?? 4000), port: null, rateHz },
    session,
  });
  await new Promise((r) => setTimeout(r, (seconds * 1000) / 2));
  // One read outside the watch list, as the Tasks view makes
  const tasks = await server
    .call<{ tasks: { error: string | null }[]; hasStats: boolean }>("session_task_states")
    .then((t) => `${t.tasks.length} tasks, ${t.tasks.filter((x) => x.error).length} unreadable, counters ${t.hasStats}`)
    .catch((e) => `failed: ${e.message}`);
  await new Promise((r) => setTimeout(r, (seconds * 1000) / 2));
  const elapsed = (Date.now() - started) / 1000;
  await server.call("session_disconnect");
  await new Promise((r) => setTimeout(r, 100));
  await server.dispose();

  const stats = lastStats as Record<string, unknown> | null;
  console.log(`states: ${states.join(" -> ")}`);
  console.log(`frames: ${frames}, ticks: ${ticks} in ${elapsed.toFixed(1)} s (${(ticks / elapsed).toFixed(1)} Hz overall)`);
  if (stats) {
    console.log(
      `last stats: target ${stats.targetHz} Hz, achieved ${Number(stats.achievedHz).toFixed(1)} Hz, ` +
        `read avg ${stats.readAvgUs} us max ${stats.readMaxUs} us, jitter ${stats.jitterUs} us, ` +
        `regions ${stats.regions}, skipped ${stats.skippedTicks}, failed regions ${stats.failedRegions}, ` +
        `dropped frames ${stats.framesDropped}, core ${stats.core}`,
    );
  }
  console.log(`task states: ${tasks}`);
  console.log(`tuning table check: ${check}`);
  console.log(`log lines: ${logLines}; errors: ${errors.size ? [...errors].join("; ") : "none"}`);
  for (const [id, value] of [...last].slice(0, 8)) console.log(`  ${leaves[id - 1].path} = ${value}`);
  const failed = states.some((s) => s.startsWith("failed"));
  if (!frames || failed) process.exit(1);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
