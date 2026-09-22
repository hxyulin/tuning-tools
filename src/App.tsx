import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { OpenedElf, SymbolNode, refForPath } from "./elf/api";
import { SymbolTree } from "./elf/SymbolTree";
import { NodeDetails } from "./elf/NodeDetails";
import { ConnectDefaults, HostStartup, WatchSeed, host } from "./host";
import type { ConnectRequest } from "./live/api";
import { CanPanel } from "./can/CanPanel";
import { LiveWatch } from "./live/LiveWatch";
import { ConnectBar } from "./live/ConnectBar";
import { LogFilter, LogTools, LogView, defaultLogFilter } from "./live/LogConsole";
import { Scope } from "./live/Scope";
import { StatusBar } from "./live/StatusBar";
import { TasksView } from "./live/TasksView";
import { TunePanel } from "./live/TunePanel";
import { NewWatch, Watch, useWatches } from "./live/useWatches";
import { useSession } from "./live/useSession";
import { Tab, TabStrip, button, ghostButton, primaryButton } from "./ui";

const DOCK_KEY = "dock";
const DOCK_MIN = 90;

function fileName(path: string) {
  return path.split(/[\\/]/).pop() ?? path;
}

function loadDock(): number {
  const stored = Number(host.storage.get(DOCK_KEY));
  return Number.isFinite(stored) && stored >= DOCK_MIN ? stored : 220;
}

/** Symbol paths from the host's launch configuration, as the numbers they name */
async function resolveSeeds(elf: OpenedElf, seeds: WatchSeed[]): Promise<NewWatch[]> {
  const out: NewWatch[] = [];
  for (const seed of seeds) {
    const ref = refForPath(elf.roots, seed.path);
    if (!ref) continue;
    const leaves = await host.watchableLeaves(ref).catch(() => []);
    for (const n of leaves) {
      out.push({
        ref: n.ref,
        cell: null,
        path: n.path,
        typeName: n.typeName,
        scalar: n.scalar,
        unit: seed.unit ?? null,
        plotted: seed.plotted,
      });
    }
  }
  return out;
}

const chipTone = {
  matches: { text: "matches target", className: "border-good text-good" },
  checking: { text: "checking target…", className: "border-rule text-muted" },
  differs: { text: "differs from target", className: "border-danger text-danger" },
};

export default function App() {
  const [canOpen, setCanOpen] = useState(false);
  const [elf, setElf] = useState<OpenedElf | null>(null);
  const [selected, setSelected] = useState<SymbolNode | null>(null);
  const [loading, setLoading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sideTab, setSideTab] = useState<"symbols" | "tune" | "live">("symbols");
  const [dockTab, setDockTab] = useState<"log" | "tasks">("log");
  const [side, setSide] = useState(host.name !== "vscode");
  const [dock, setDock] = useState(loadDock);
  const [dockOpen, setDockOpen] = useState(false);
  const [logFilter, setLogFilter] = useState<LogFilter>(defaultLogFilter);
  const [preset, setPreset] = useState<ConnectRequest | null>(null);
  const [connectDefaults, setConnectDefaults] = useState<ConnectDefaults | null>(null);
  const startup = useRef<HostStartup | null>(null);
  const session = useSession();
  // A framed link lists its own values, so it works without an ELF
  const linkOnly = elf === null && session.catalog !== null;
  const watch = useWatches(elf?.summary.path ?? (linkOnly ? "link" : null), elf);
  const catalog = session.catalog ?? elf?.catalog ?? null;
  const hasTasks = (elf?.tasks.length ?? 0) > 0;
  const tab = linkOnly || !elf ? "tune" : sideTab;
  const dockView = hasTasks ? dockTab : "log";
  const connected = session.link.state === "connected";
  const check = session.tune?.check ?? null;
  // Task slots stand in for their untyped pools, so the raw storage stays browsable
  const symbolRoots = useMemo(() => {
    if (!elf?.tasks.length) return elf?.roots ?? [];
    const pools = new Set(elf.tasks.map((t) => t.root.ref.symbol));
    return [...elf.roots.filter((r) => !pools.has(r.path)), ...elf.tasks.map((t) => ({ ...t.root, internal: true }))];
  }, [elf]);

  const loadElf = useCallback(async (path: string) => {
    setLoading(fileName(path));
    setError(null);
    try {
      const opened = await host.openElf(path);
      setElf(opened);
      setSelected(null);
      setSideTab(opened.catalog ? "tune" : "symbols");
      return opened;
    } catch (e) {
      setError(`Could not open ${fileName(path)}: ${e}`);
      return null;
    } finally {
      setLoading(null);
    }
  }, []);

  async function chooseElf() {
    try {
      const path = await host.pickElf();
      if (path) await loadElf(path);
    } catch (e) {
      setError(String(e));
    }
  }

  const [startupRevision, setStartupRevision] = useState(0);
  useEffect(() => host.watchStartup?.(() => setStartupRevision((n) => n + 1)), []);

  const { connect } = session;
  useEffect(() => {
    let stale = false;
    host.startup().then(async (s) => {
      if (stale) return;
      startup.current = s;
      setConnectDefaults(s.connectDefaults);
      const opened = s.elfPath ? await loadElf(s.elfPath) : null;
      if ((opened || !s.elfPath) && s.connect && !stale) {
        setPreset(s.connect);
        void connect(s.connect);
      }
    }).catch((e) => { if (!stale) setError(`Could not restore the target: ${e}`); });
    return () => {
      stale = true;
    };
  }, [loadElf, connect, startupRevision]);

  // Launch watches fill a list that has nothing saved, once the list for the ELF is loaded
  const { seed, ready } = watch;
  useEffect(() => {
    const seeds = startup.current?.watches;
    if (!elf || !ready || !seeds?.length || elf.summary.path !== startup.current?.elfPath) return;
    startup.current = { ...startup.current, watches: [] };
    resolveSeeds(elf, seeds).then(seed);
  }, [elf, ready, seed]);

  const { add } = watch;
  const onWatch = useCallback(
    (node: SymbolNode) => {
      if (node.kind === "scalar" || node.kind === "enum") {
        add([node]);
        return;
      }
      host.watchableLeaves(node.ref).then(
        (leaves) => {
          if (leaves.length === 0) setError(`${node.path} holds no numbers that can be sampled.`);
          else add(leaves);
        },
        (e) => setError(`Could not watch ${node.path}: ${e}`),
      );
    },
    [add],
  );

  useEffect(() => {
    if (!ready) return;
    return host.watchRequests?.((nodes) => nodes.forEach(onWatch));
  }, [ready, onWatch]);

  const workbench = (command: string) => {
    void host.workbenchCommand?.(command).catch((e) => setError(String(e)));
  };

  const writable = useCallback(
    (w: Watch) => {
      const entry = w.cell === null ? undefined : catalog?.entries.find((e) => e.id === w.cell);
      return connected && check?.state === "matches" && entry !== undefined && entry.access !== "readOnly";
    },
    [catalog, connected, check],
  );
  const onWrite = useCallback((w: Watch, value: number) => host.requestValue(w.cell ?? -1, value), []);

  const resizeDock = (height: number) => {
    const next = Math.max(DOCK_MIN, Math.min(height, window.innerHeight - 220));
    setDock(next);
    try {
      host.storage.set(DOCK_KEY, String(Math.round(next)));
    } catch {
      // Not remembered next launch
    }
  };

  const watchedPaths = useMemo(() => new Set(watch.watches.map((w) => w.path)), [watch.watches]);
  const chip = elf && connected && check ? chipTone[check.state] : null;

  return (
    <div className="flex h-full flex-col">
      {host.name !== "vscode" && (
        <>
          <header className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-2 border-b border-rule bg-surface px-4 py-2">
            <span className="mr-2 font-semibold tracking-tight">Tuning Studio</span>
            <button onClick={chooseElf} disabled={loading !== null || connected} title={connected ? "Disconnect before changing firmware" : undefined} className={button}>
              Open ELF…
            </button>
            {loading && <span className="text-muted">Reading {loading}…</span>}
            {!loading && elf && (
              <span className="flex min-w-0 items-center gap-2">
                <span className="truncate font-mono text-[13px] font-medium" title={elf.summary.path}>
                  {fileName(elf.summary.path)}
                </span>
                {chip && (
                  <span
                    title={check?.state === "differs" ? check.message : undefined}
                    className={`shrink-0 rounded-full border px-2 text-[11px] leading-[17px] ${chip.className}`}
                  >
                    {chip.text}
                  </span>
                )}
              </span>
            )}
            <span className="flex-1" />
            <button className={ghostButton} aria-expanded={side} onClick={() => setSide((s) => !s)}>Variables</button>
            <button className={ghostButton} aria-expanded={dockOpen} onClick={() => setDockOpen((s) => !s)}>Logs & tasks</button>
          </header>
          <div className="shrink-0 border-b border-rule bg-panel px-4 py-2">
            <ConnectBar
              link={session.link}
              canConnect={elf !== null}
              preset={preset}
              defaults={connectDefaults}
              onConnect={(request) => void session.connect(request)}
              onDisconnect={() => void session.disconnect()}
            />
          </div>
        </>
      )}
      {host.name === "vscode" && (
        <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-rule px-3 py-1.5">
          <button className={ghostButton} onClick={() => workbench("tuningStudio.target.focus")}>Target & symbols</button>
          <span className="min-w-0 flex-1 truncate text-muted">{elf ? fileName(elf.summary.path) : connected ? "USB tuning" : "No target connected"}</span>
          {chip && <span className={`text-[11px] ${chip.className}`}>{chip.text}</span>}
          <button className={ghostButton} aria-expanded={side} onClick={() => { setSide((s) => !s); setSideTab("tune"); }}>Tune</button>
          {elf && <button className={ghostButton} aria-expanded={side && sideTab === "live"} onClick={() => { setSide(true); setSideTab("live"); }}>Live Watch</button>}
          <button className={ghostButton} onClick={() => workbench("tuningStudio.showLogs")}>Firmware log</button>
          {hasTasks && <button className={ghostButton} onClick={() => { setDockTab("tasks"); setDockOpen((s) => !s); }}>Tasks</button>}
          <button className={button} onClick={() => workbench(connected ? "tuningStudio.disconnect" : "tuningStudio.connect")}>{connected ? "Disconnect" : "Connect…"}</button>
        </div>
      )}

      {error && (
        <div role="alert" className="flex items-center border-b border-rule bg-surface px-3 py-1.5 text-danger">
          <span className="flex-1">{error}</span>
          <button onClick={() => setError(null)} className={ghostButton}>
            Dismiss
          </button>
        </div>
      )}

      <div className="flex shrink-0 gap-2 border-b border-rule bg-panel px-3 py-1">
        <button className={canOpen ? ghostButton : primaryButton} onClick={() => setCanOpen(false)}>Firmware</button>
        <button className={canOpen ? primaryButton : ghostButton} onClick={() => setCanOpen(true)}>CAN bus</button>
      </div>
      <div className={canOpen ? "flex min-h-0 flex-1 flex-col" : "hidden"}><CanPanel /></div>
      {canOpen ? null : elf || linkOnly ? (
        <main
          className={`grid min-h-0 flex-1 ${side ? "workspace-with-sidebar" : "grid-cols-[minmax(0,1fr)]"}`}
        >
          {side && (
            <aside className="flex min-h-0 min-w-0 flex-col border-r border-rule bg-surface">
              <div className="px-3 pt-3 pb-2 text-[11px] font-semibold uppercase tracking-wider text-muted">Variables</div>
              <TabStrip>
                {elf && (
                  <Tab selected={tab === "symbols"} onSelect={() => setSideTab("symbols")} count={symbolRoots.length}>
                    Symbols
                  </Tab>
                )}
                {elf && <Tab selected={tab === "live"} onSelect={() => setSideTab("live")}>Live Watch</Tab>}
                <Tab selected={tab === "tune"} onSelect={() => setSideTab("tune")} count={catalog?.entries.length ?? null}>
                  Tune
                </Tab>
              </TabStrip>
              {tab === "live" && elf ? (
                <div className="min-h-0 flex-1">
                  <LiveWatch catalog={catalog} tune={session.tune} onWatchCell={watch.addCell} roots={symbolRoots} connected={connected} carrier={session.link.carrier} onWatch={onWatch} watched={watchedPaths} />
                </div>
              ) : tab === "symbols" && elf ? (
                <>
                  <div className="min-h-0 flex-1">
                    <SymbolTree
                      roots={symbolRoots}
                      selected={selected}
                      onSelect={setSelected}
                      onWatch={onWatch}
                      watched={watchedPaths}
                    />
                  </div>
                  <div className="max-h-[40%] shrink-0 overflow-auto border-t border-rule">
                    <NodeDetails node={selected} roots={elf.roots} onWatch={onWatch} />
                  </div>
                </>
              ) : (
                <div className="min-h-0 flex-1">
                  <TunePanel
                    catalog={catalog}
                    catalogError={elf?.catalogError ?? null}
                    fromTarget={session.catalog !== null}
                    tune={session.tune}
                    connected={connected}
                    watched={watchedPaths}
                    onWatch={watch.addCell}
                    onSave={session.save}
                  />
                </div>
              )}
            </aside>
          )}
          <div
            className="grid min-h-0 min-w-0 grid-rows-[minmax(0,1fr)_auto_var(--dock)]"
            style={{ "--dock": dockOpen ? `min(${dock}px, 40vh)` : "0px" } as React.CSSProperties}
          >
            <Scope
              watches={watch.watches}
              connected={connected}
              halted={connected && session.stats?.core === "halted"}
              onToggleSide={() => { setSide(true); if (host.name === "vscode") setSideTab("tune"); }}
              onTogglePlot={watch.togglePlot}
              onRemove={watch.remove}
              onClear={watch.clear}
              onSetUnit={watch.setUnit}
              writable={writable}
              onWrite={onWrite}
            />
            <div
              role="separator"
              aria-orientation="horizontal"
              aria-label="Resize the bottom panel"
              aria-valuenow={dock}
              tabIndex={0}
              onPointerDown={(e) => {
                const el = e.currentTarget;
                el.setPointerCapture(e.pointerId);
                const start = e.clientY;
                const from = dock;
                const move = (m: PointerEvent) => resizeDock(from + start - m.clientY);
                const up = () => {
                  el.removeEventListener("pointermove", move);
                  el.removeEventListener("pointerup", up);
                };
                el.addEventListener("pointermove", move);
                el.addEventListener("pointerup", up);
              }}
              onKeyDown={(e) => {
                if (e.key === "ArrowUp") resizeDock(dock + 24);
                else if (e.key === "ArrowDown") resizeDock(dock - 24);
                else return;
                e.preventDefault();
              }}
              className={`${dockOpen ? "h-[5px]" : "hidden"} cursor-row-resize border-t border-rule bg-panel hover:bg-accent-wash focus-visible:bg-accent-wash`}
            />
            <section aria-label="Bottom panel" className={`${dockOpen ? "flex" : "hidden"} min-h-0 flex-col overflow-hidden bg-surface`}>
              <TabStrip
                tools={
                  <>
                    {dockView === "log" && <LogTools filter={logFilter} onChange={setLogFilter} onClear={session.clearLogs} />}
                    <button className={ghostButton} onClick={() => setDockOpen(false)} aria-label="Hide bottom panel">×</button>
                  </>
                }
              >
                <Tab selected={dockView === "log"} onSelect={() => setDockTab("log")}>
                  Log
                </Tab>
                {hasTasks && (
                  <Tab selected={dockView === "tasks"} onSelect={() => setDockTab("tasks")} count={elf?.tasks.length}>
                    Tasks
                  </Tab>
                )}
              </TabStrip>
              {dockView === "tasks" && elf ? (
                <div className="min-h-0 flex-1">
                  <TasksView
                    tasks={elf.tasks}
                    connected={connected}
                    carrier={session.link.carrier}
                    onWatch={onWatch}
                    watched={watchedPaths}
                  />
                </div>
              ) : (
                <LogView
                  lines={session.logs}
                  stream={session.stats?.log ?? null}
                  connected={connected}
                  filter={logFilter}
                  onFollowChange={(follow) => setLogFilter((f) => ({ ...f, follow }))}
                />
              )}
            </section>
          </div>
        </main>
      ) : (
        <main className="flex flex-1 items-center justify-center bg-surface p-8">
          <div className="max-w-md">
            <h1 className="text-[20px] font-semibold">Start watching your firmware</h1>
            <p className="mt-2 leading-relaxed text-muted">
              Open a firmware ELF to browse variables and plot them through your debug probe.
              {host.name === "vscode" ? " Choose Connect… above for a USB target — no ELF needed." : " For a USB target, choose USB in Connection settings above and connect directly — no ELF needed."}
            </p>
            <button onClick={chooseElf} disabled={loading !== null} className={`${primaryButton} mt-4`}>
              Open ELF…
            </button>
          </div>
        </main>
      )}

      <StatusBar elf={elf} link={session.link} stats={session.stats} />
    </div>
  );
}
