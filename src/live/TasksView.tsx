import { useEffect, useMemo, useRef, useState } from "react";
import { RootNode, SymbolNode, Task, TaskPoint, hex, shortLocation } from "../elf/api";
import { RowNote, SymbolTree } from "../elf/SymbolTree";
import { host } from "../host";
import { Carrier, TaskSnapshot, TaskState, TaskStatus, ValueRead } from "./api";
import { TaskTimeline } from "./TaskTimeline";
import { formatValue } from "./format";

/** Between reads of task states and locals; people read these, they do not plot them */
const POLL_MS = 500;

/** Run `read` every POLL_MS while `live`, one read in flight at a time. */
function usePoll<T>(live: boolean, read: (() => Promise<T>) | null, deps: unknown[]) {
  const [visible, setVisible] = useState(!document.hidden);
  useEffect(() => {
    const change = () => setVisible(!document.hidden);
    document.addEventListener("visibilitychange", change);
    return () => document.removeEventListener("visibilitychange", change);
  }, []);
  const [value, setValue] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    setValue(null);
    setError(null);
    if (!live || !visible || !read) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = () =>
      read()
        .then(
          (v) => {
            if (stopped) return;
            setValue(v);
            setError(null);
          },
          (e) => { if (!stopped) { setValue(null); setError(String(e)); } },
        )
        .finally(() => {
          if (!stopped) timer = setTimeout(poll, POLL_MS);
        });
    void poll();
    return () => {
      stopped = true;
      clearTimeout(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [live, visible, ...deps]);
  return { value, error };
}

type Tone = "ink" | "muted" | "warn" | "danger";
const toneClass: Record<Tone, string> = {
  ink: "text-ink",
  muted: "text-muted",
  warn: "text-ink",
  danger: "text-danger",
};
const dotClass: Record<Tone, string> = {
  ink: "bg-faint",
  muted: "border border-faint",
  warn: "bg-warn",
  danger: "bg-danger",
};

/** What a task is doing, in a word, and why */
function describe(status: TaskStatus | undefined): { text: string; tone: Tone; title?: string } {
  if (!status) return { text: "", tone: "muted" };
  const { state } = status;
  if (!state) return { text: "unreadable", tone: "danger", title: status.error ?? undefined };
  if (!state.spawned) return { text: "not running", tone: "muted", title: "Not spawned, or already finished" };
  const label = state.at?.label;
  if (label === "Unresumed") return { text: "not started", tone: "warn", title: "Spawned, not yet polled" };
  if (label === "Returned") return { text: "returned", tone: "muted" };
  if (label === "Panicked") return { text: "panicked", tone: "danger" };
  return state.queued
    ? { text: "ready", tone: "warn", title: "Woken, waiting in the run queue to be polled" }
    : { text: "waiting", tone: "ink", title: "Parked on an .await until something wakes it" };
}

function fullLocation(at: TaskPoint) {
  return at.location ? `${at.location.file}:${at.location.line}` : at.label;
}

/** States that hold data: the arguments before the first poll, and the locals at each `.await` */
function hasLocals(p: TaskPoint) {
  return p.label !== "Returned" && p.label !== "Panicked";
}

/** Rates over the last few reads of a task's counters */
interface Load {
  /** Share of wall time spent polling the task, 0–100 */
  cpu: number;
  pollsPerSec: number;
  /** Mean poll over the window, in µs; null without polls */
  avgUs: number | null;
  recentMaxUs: number;
  maxUs: number;
}

/** Reads kept to average over: about two seconds at POLL_MS */
const WINDOW = 5;
/** Older reads may have seen the 32-bit cycle count wrap more than once */
const MAX_WINDOW_US = 4_000_000;

function loads(history: TaskSnapshot[]): Map<string, Load> {
  const out = new Map<string, Load>();
  const now = history[history.length - 1];
  const then = history[0];
  const hz = now?.clockHz;
  if (!now || !hz) return out;
  const dt = (now.hostUs - then.hostUs) / 1e6;
  const before = new Map(then.tasks.map((t) => [t.path, t.counters]));
  for (const t of now.tasks) {
    const c = t.counters;
    if (!c) continue;
    const us = (cycles: number) => (cycles / hz) * 1e6;
    const load: Load = { cpu: 0, pollsPerSec: 0, avgUs: null, recentMaxUs: us(c.recentMaxCycles), maxUs: us(c.maxCycles) };
    const b = before.get(t.path);
    // A respawned task starts again from zero; wait for two reads of the new one
    if (b && dt > 0 && c.polls >= b.polls) {
      const polls = c.polls - b.polls;
      const cycles = (c.cycles - b.cycles) >>> 0;
      load.cpu = (cycles / (dt * hz)) * 100;
      load.pollsPerSec = polls / dt;
      load.avgUs = polls ? us(cycles / polls) : null;
    }
    out.set(t.path, load);
  }
  return out;
}

function percent(p: number) {
  return p === 0 ? "0 %" : p < 0.1 ? "<0.1 %" : `${p.toFixed(1)} %`;
}

function rate(r: number) {
  return r === 0 ? "0" : r < 10 ? r.toFixed(1) : Math.round(r).toLocaleString();
}

function micros(us: number) {
  if (us >= 1000) return `${(us / 1000).toFixed(us >= 10_000 ? 0 : 1)} ms`;
  return us < 10 ? `${us.toFixed(1)} µs` : `${Math.round(us)} µs`;
}

function bytes(n: number | null) {
  return n === null ? "" : n >= 1024 ? `${(n / 1024).toFixed(1)} KiB` : `${n} B`;
}

interface Props {
  tasks: Task[];
  connected: boolean;
  carrier: Carrier | null;
  onWatch: (node: SymbolNode) => void;
  watched: Set<string>;
}

export function TasksView({ tasks, connected, carrier, onWatch, watched }: Props) {
  const live = connected && carrier === "probe";
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [byCpu, setByCpu] = useState(false);
  const [timeline, setTimeline] = useState(false);
  const { value: snapshot, error } = usePoll(live, host.taskStates, [tasks]);
  const statuses = snapshot?.tasks ?? null;
  const [samples, setSamples] = useState<TaskSnapshot[]>([]);

  // Recent reads, for rates; restarted when the ELF, link or clock changes
  const history = useRef<TaskSnapshot[]>([]);
  const [load, setLoad] = useState<Map<string, Load>>(new Map());
  useEffect(() => {
    history.current = [];
    setSamples([]);
    setLoad(new Map());
  }, [tasks, live]);
  useEffect(() => {
    if (!snapshot) { history.current = []; setLoad(new Map()); setSamples([]); return; }
    setSamples((previous) => [...previous.filter((s) => s.hostUs < snapshot.hostUs && snapshot.hostUs - s.hostUs < 30_000_000), snapshot].slice(-60));
    const kept = history.current.filter(
      (h) => h.clockHz === snapshot.clockHz && snapshot.hostUs - h.hostUs <= MAX_WINDOW_US,
    );
    history.current = [...kept, snapshot].slice(-WINDOW);
    setLoad(loads(history.current));
  }, [snapshot]);

  const byPath = useMemo(() => new Map((statuses ?? []).map((s) => [s.path, s])), [statuses]);
  const selected = tasks.find((t) => t.root.path === selectedPath) ?? null;
  const hasStats = snapshot?.hasStats ?? false;
  const rows = useMemo(
    () => (byCpu ? [...tasks].sort((a, b) => (load.get(b.root.path)?.cpu ?? -1) - (load.get(a.root.path)?.cpu ?? -1)) : tasks),
    [tasks, byCpu, load],
  );

  const running = statuses?.filter((s) => s.state?.spawned).length ?? 0;
  const ready = statuses?.filter((s) => s.state?.queued).length ?? 0;
  const ram = tasks.reduce((sum, t) => sum + (t.futureSize ?? 0), 0);
  const busy = [...load.values()].reduce((sum, l) => sum + l.cpu, 0);
  let summary: React.ReactNode;
  if (carrier === "serial" && connected) summary = "Task states need a debug probe; the USB link cannot read memory.";
  else if (!live) summary = "Connect a debug probe to see what each task is doing.";
  else if (error) summary = <span className="text-danger">Could not read task states: {error}</span>;
  else if (statuses) {
    summary = `${running} of ${tasks.length} running, ${ready} ready to be polled`;
    if (history.current.length > 1 && load.size) summary += `, ${percent(busy)} of the CPU in tasks`;
  } else summary = "Reading task states…";

  let statsNote: React.ReactNode = null;
  if (live && snapshot) {
    if (!snapshot.hasStats) {
      statsNote = "Link rm-task-stats in the firmware to see each task's CPU time.";
    } else if (snapshot.statsError) {
      statsNote = <span className="text-danger">Could not read task CPU time: {snapshot.statsError}</span>;
    } else if (snapshot.clockHz === 0) {
      statsNote = "The firmware has not called rm_task_stats::init yet, so poll times are unknown.";
    } else if (snapshot.untracked > 0) {
      statsNote = `${snapshot.untracked} task${snapshot.untracked === 1 ? " was" : "s were"} spawned after every counter slot was taken and are not counted.`;
    }
  }

  return (
    <div className="@container flex h-full min-h-0 flex-col text-[12px]">
      <div className="flex items-center gap-3 border-b border-grid px-3 py-1">
        <button className="shrink-0 text-accent" onClick={() => setTimeline((v) => !v)}>{timeline ? "Tasks" : "Execution timeline"}</button>
        <span className="min-w-0 truncate text-muted tabular-nums">{summary}</span>
        <span className="ml-auto shrink-0 text-muted tabular-nums" title="Sum of every task's future">
          {bytes(ram)} in futures
        </span>
      </div>
      {timeline ? <TaskTimeline tasks={tasks} live={live} select={(path) => { setSelectedPath(path); setTimeline(false); }} /> : <div className={`grid min-h-0 flex-1 ${selected ? "grid-rows-2 @min-[900px]:grid-rows-1 @min-[900px]:grid-cols-[minmax(0,1fr)_minmax(280px,42%)]" : "grid-cols-1"}`}>
        <div className="min-h-0 overflow-auto">
          <table className="min-w-[600px] w-full table-fixed border-collapse text-[12px]">
            <colgroup>
              <col />
              <col className="w-[13%]" />
              <col className="w-[17%]" />
              {hasStats && (
                <>
                  <col className="w-[11%]" />
                  <col className="w-[10%]" />
                  <col className="w-[12%]" />
                </>
              )}
              <col className="w-[10%]" />
            </colgroup>
            <thead className="sticky top-0 bg-surface text-left text-muted">
              <tr className="border-b border-rule">
                <th className="py-1 pl-3 font-medium">
                  <button onClick={() => setByCpu(false)} className={byCpu ? "hover:text-ink" : "text-ink"}>
                    Task
                  </button>
                </th>
                <th className="py-1 pl-2 font-medium">State</th>
                <th className="py-1 pl-2 font-medium">Parked at</th>
                {hasStats && (
                  <>
                    <th className="py-1 pr-2 text-right font-medium" title="Share of time spent polling the task, over the last two seconds. Interrupts during a poll count toward it.">
                      <button onClick={() => setByCpu(true)} className={byCpu ? "text-ink" : "hover:text-ink"}>
                        CPU{byCpu && " ▾"}
                      </button>
                    </th>
                    <th className="py-1 pr-2 text-right font-medium" title="Polls per second: how often the task wakes and runs">
                      Polls/s
                    </th>
                    <th className="py-1 pr-2 text-right font-medium" title="Longest single poll in the last one to two seconds: the longest the task kept every other task waiting">
                      Longest
                    </th>
                  </>
                )}
                <th className="py-1 pr-3 text-right font-medium" title="The async fn's future: its arguments and the locals it keeps across an .await">
                  Future
                </th>
              </tr>
            </thead>
            <tbody>
              {rows.map((t) => {
                const status = byPath.get(t.root.path);
                const l = load.get(t.root.path);
                const counted = history.current.length > 1;
                const state = describe(status);
                const at = status?.state?.spawned ? status.state.at : null;
                const isSelected = t.root.path === selectedPath;
                const module = t.root.segments.slice(0, -1).join("::");
                return (
                  <tr
                    key={t.root.path}
                    aria-selected={isSelected}
                    tabIndex={0}
                    aria-label={`${t.root.label}: ${state.text || "not connected"}`}
                    onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setSelectedPath(isSelected ? null : t.root.path); } }}
                    onClick={() => setSelectedPath(isSelected ? null : t.root.path)}
                    className={`cursor-default border-b border-grid ${isSelected ? "bg-accent-wash" : "hover:bg-panel"}`}
                  >
                    <td className="truncate py-1 pl-3" title={t.root.path}>
                      <span className="font-mono">{t.root.label}</span>
                      {module && <span className="pl-2 text-[11px] text-muted">{module}</span>}
                    </td>
                    <td className={`truncate py-1 pl-2 ${toneClass[state.tone]}`} title={state.title}>
                      {state.text && (
                        <span aria-hidden className={`mr-1.5 inline-block h-1.5 w-1.5 rounded-full align-middle ${dotClass[state.tone]}`} />
                      )}
                      {state.text}
                    </td>
                    <td className="truncate py-1 pl-2 font-mono text-muted" title={at ? `${fullLocation(at)} (${at.label})` : undefined}>
                      {at?.location && at.label.startsWith("Suspend") ? shortLocation(at.location) : ""}
                    </td>
                    {hasStats && (
                      <>
                        <td className="whitespace-nowrap py-1 pr-2 text-right font-mono tabular-nums">
                          {l && counted && (
                            <>
                              <span aria-hidden className="mr-1.5 inline-block h-1.5 w-[20%] overflow-hidden rounded-[1px] bg-grid align-middle">
                                {/* Same 0–100% scale for every task. */}
                                <span className="block h-full bg-muted" style={{ width: `${Math.min(l.cpu, 100)}%` }} />
                              </span>
                              {percent(l.cpu)}
                            </>
                          )}
                        </td>
                        <td className="truncate py-1 pr-2 text-right font-mono tabular-nums">
                          {l && counted ? rate(l.pollsPerSec) : ""}
                        </td>
                        <td
                          className="truncate py-1 pr-2 text-right font-mono tabular-nums"
                          title={l ? `${micros(l.maxUs)} since the task was spawned` : undefined}
                        >
                          {l && l.recentMaxUs > 0 ? micros(l.recentMaxUs) : ""}
                        </td>
                      </>
                    )}
                    <td className="truncate py-1 pr-3 text-right font-mono tabular-nums text-muted">{bytes(t.futureSize)}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          {statsNote && <p className="px-3 py-2 text-muted">{statsNote}</p>}
        </div>
        {selected && (
          <TaskDetail
            key={selected.root.path}
            task={selected}
            samples={samples}
            state={byPath.get(selected.root.path)?.state ?? null}
            load={history.current.length > 1 ? (load.get(selected.root.path) ?? null) : null}
            live={live}
            onWatch={onWatch}
            watched={watched}
            onClose={() => setSelectedPath(null)}
          />
        )}
      </div>}
    </div>
  );
}

interface DetailProps {
  task: Task;
  samples: TaskSnapshot[];
  state: TaskState | null;
  load: Load | null;
  live: boolean;
  onWatch: (node: SymbolNode) => void;
  watched: Set<string>;
  onClose: () => void;
}

function TaskDetail({ task, samples, state, load, live, onWatch, watched, onClose }: DetailProps) {
  const points = task.states.filter(hasLocals);
  const current = state?.spawned ? state.at : null;
  // Follow the task from await to await until a state is picked by hand
  const [pinned, setPinned] = useState<string | null>(null);
  const shown = points.find((p) => p.path === (pinned ?? current?.path)) ?? points[0] ?? null;
  const stale = live && shown !== null && current?.path !== shown.path;
  const observations = useMemo(() => {
    const runs: { key: string; label: string; title: string; count: number; point: TaskPoint | null; color: string }[] = [];
    for (const sample of samples) {
      const status = sample.tasks.find((s) => s.path === task.root.path);
      const description = describe(status);
      const point = status?.state?.spawned ? status.state.at : null;
      const key = `${description.text}:${point?.path ?? ""}`;
      const previous = runs[runs.length - 1];
      if (previous?.key === key) { previous.count++; continue; }
      const age = ((samples[samples.length - 1].hostUs - sample.hostUs) / 1e6).toFixed(1);
      const index = task.states.findIndex((p) => p.path === point?.path);
      runs.push({ key, count: 1, point, label: point?.label ?? description.text,
        title: `${age}s ago: ${description.text}${point ? ` · ${fullLocation(point)} (${point.label})` : ""}`,
        color: description.tone === "danger" ? "bg-danger/30" : status?.state?.queued ? "bg-warn/30" : status?.state?.spawned ? index % 2 ? "bg-accent/30" : "bg-accent/60" : "bg-grid" });
    }
    return runs;
  }, [samples, task]);

  return (
    <aside className="flex min-h-0 flex-col border-t @min-[900px]:border-t-0 @min-[900px]:border-l border-rule">
      <div className="flex items-start gap-2 border-b border-rule px-3 py-2">
        <div className="min-w-0 flex-1">
          <h3 className="truncate font-mono text-[13px]" title={task.root.path}>
            {task.root.label}
          </h3>
          <p className="truncate text-[11px] text-muted">
            {task.root.segments.slice(0, -1).join("::")}
            {task.slots > 1 && ` · slot ${task.slot + 1} of ${task.slots}`} · at {hex(task.root.address)}
          </p>
          {load && (
            <p className="mt-1 font-mono text-[11px] tabular-nums">
              {percent(load.cpu)} CPU · {rate(load.pollsPerSec)} polls/s
              {load.avgUs !== null && ` · ${micros(load.avgUs)} per poll`} · longest {micros(load.recentMaxUs)}
              <span className="text-muted"> ({micros(load.maxUs)} since spawn)</span>
            </p>
          )}
        </div>
        <button onClick={onClose} aria-label="Close task" className="rounded-sm px-1 text-muted hover:bg-sunken hover:text-ink">
          ×
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-auto">
        {samples.length > 0 && <section className="border-b border-rule px-3 py-2" aria-label="Sampled task history">
          <div className="mb-1 flex justify-between text-[11px] text-muted"><span>Recent observations</span><span>now →</span></div>
          <ol className="flex h-5 gap-px" aria-label="Task state samples">
            {observations.map((run, index) => <li key={index} className="min-w-0" style={{ flexGrow: run.count, flexBasis: 0 }}><button
              title={run.title} aria-label={run.title}
              disabled={!run.point || !hasLocals(run.point)}
              onClick={() => setPinned(run.point?.path === current?.path ? null : run.point?.path ?? null)}
              className={`h-full w-full min-w-0 truncate rounded-[1px] px-0.5 font-mono text-[9px] ${run.color}`}
            >{run.label}</button></li>)}
          </ol>
          <p className="mt-1 text-[10px] text-muted">500 ms samples, not a scheduler trace. Brief transitions may be missed.</p>
        </section>}
        <div className="flex items-center justify-between px-3 pt-2 pb-1">
          <h4 className="text-[11px] text-muted">Await points</h4>
          {pinned ? <button className="text-accent hover:underline" onClick={() => setPinned(null)}>Follow current state</button> : <span className="text-[11px] text-muted">Following current state</span>}
        </div>
        <ul className="px-1.5">
          {points.map((p) => {
            const isCurrent = p.path === current?.path;
            const isShown = p.path === shown?.path;
            return (
              <li key={p.path}>
                <button
                  onClick={() => setPinned(isCurrent ? null : p.path)}
                  aria-pressed={isShown}
                  title={p.location ? fullLocation(p) : undefined}
                  className={`flex w-full items-center gap-2 rounded-sm px-1.5 py-0.5 text-left ${
                    isShown ? "bg-sunken" : "hover:bg-panel"
                  }`}
                >
                  <span className={`w-2 text-[9px] ${isCurrent ? "text-accent" : "invisible"}`} aria-hidden>
                    ●
                  </span>
                  <span className="font-mono text-[12px]">
                    {p.label === "Unresumed" ? "before first poll" : p.location ? shortLocation(p.location) : p.label}
                  </span>
                  <span className="ml-auto text-[11px] text-muted">
                    {isCurrent ? (state?.queued ? "current, ready" : "current") : p.label === "Unresumed" ? "arguments" : p.label}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>

        {shown && (
          <Locals
            key={shown.path}
            point={shown}
            live={live}
            stale={stale}
            onWatch={onWatch}
            watched={watched}
          />
        )}
      </div>
    </aside>
  );
}

interface LocalsProps {
  point: TaskPoint;
  live: boolean;
  /** The task is not in this state, so the bytes belong to another */
  stale: boolean;
  onWatch: (node: SymbolNode) => void;
  watched: Set<string>;
}

function Locals({ point, live, stale, onWatch, watched }: LocalsProps) {
  const [roots, setRoots] = useState<RootNode[]>([]);
  const [selected, setSelected] = useState<SymbolNode | null>(null);
  const [visible, setVisible] = useState<SymbolNode[]>([]);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    setRoots([]);
    setError(null);
    host.symbolChildren(point.ref).then((children) => {
      if (!cancelled) setRoots(children.nodes.map((node) => ({ ...node, segments: [node.label], section: "", readOnly: false, internal: false })));
    }, (e) => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, [point.ref]);
  const nodes = useMemo(() => visible.slice(0, 128), [visible]);
  const read = useMemo(() => nodes.length ? () => host.readValues(nodes.map((n) => n.ref)) : null, [nodes]);
  const { value: values, error: readError } = usePoll<ValueRead[]>(live && !stale, read, [read]);
  const notes = useMemo(() => new Map<string, RowNote>(visible.map((node, i) => {
    const value = values?.[i];
    return [node.path, {
      text: stale ? "inactive" : i >= 128 ? "limit reached" : value?.error ? "unreadable" : value?.text ?? (value?.value == null ? "—" : node.kind === "pointer" ? hex(value.value) : formatValue(value.value, node.scalar)),
      title: value?.error ?? node.typeName,
      tone: value?.error ? "danger" : stale ? "muted" : "ink",
    }];
  })), [visible, values, stale]);
  const variantKey = JSON.stringify(visible.map((node, i) => [node.path, values?.[i]?.activeVariant ?? null]));
  const variants = useMemo(() => new Map<string, string | null>(JSON.parse(variantKey)), [variantKey]);
  const where = point.label === "Unresumed" ? "Arguments, before the first poll" : `Kept across ${point.location ? shortLocation(point.location) : point.label}`;
  return <section className="mt-2 border-t border-rule">
    <h4 className="px-3 pt-2 pb-1 text-[11px] text-muted">{where}</h4>
    {stale && <p className="px-3 pb-1 text-[11px] text-warn">This state is inactive. Values are hidden because its storage may belong to another state.</p>}
    {error && <p className="px-3 text-danger">{error}</p>}
    {readError && <p className="px-3 text-danger">Could not read: {readError}</p>}
    {roots.length > 0 ? <div className="h-64 min-h-0">
      <SymbolTree roots={roots} live={live && !stale} activeVariants={variants} selected={selected} onSelect={setSelected} onWatch={stale ? undefined : onWatch} watched={watched} filters={false} label="Task locals" notes={notes} onVisibleNodes={setVisible} />
    </div> : !error && <p className="px-3 pb-2 text-muted">No locals available in this state's debug information.</p>}
    {!stale && live && <p className="px-3 py-1 text-[10px] text-muted">State and locals are sampled separately while the target runs.</p>}
  </section>;
}
