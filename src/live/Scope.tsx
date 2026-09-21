import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import { host } from "../host";
import { Segmented, Popover, button, field, ghostButton } from "../ui";
import { formatTicks } from "./format";
import { Legend, LegendActions } from "./Legend";
import { Readout, newReadout, notify, useReadout } from "./readout";
import { RecordButton, RecordNotice, useRecorder } from "./RecordControl";
import { samples } from "./samples";
import { Watch } from "./useWatches";

const WINDOWS = [2, 5, 10, 30, 60];
const SETTINGS_KEY = "scope";

type Layout = "lanes" | "overlay";

interface Settings {
  windowSec: number;
  layout: Layout;
}

function loadSettings(): Settings {
  const fallback: Settings = { windowSec: 10, layout: "lanes" };
  try {
    return { ...fallback, ...JSON.parse(host.storage.get(SETTINGS_KEY) ?? "{}") };
  } catch {
    return fallback;
  }
}

function cssVar(name: string) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** Traces that share a y-axis */
export interface Lane {
  key: string;
  /** Unit of every trace in the lane; null for traces with none, a list when overlaid */
  unit: string | null;
  watches: Watch[];
}

function laneGroups(watches: Watch[], layout: Layout): Lane[] {
  const plotted = watches.filter((w) => w.plotted && w.trace !== null && !w.error);
  if (plotted.length === 0) return [];
  if (layout === "overlay") {
    const units = [...new Set(plotted.map((w) => w.unit).filter((u) => u !== null))];
    return [{ key: "all", unit: units.length ? units.join(", ") : null, watches: plotted }];
  }
  const byUnit = new Map<string, Watch[]>();
  for (const w of plotted) {
    const key = w.unit ?? "";
    byUnit.set(key, [...(byUnit.get(key) ?? []), w]);
  }
  return [...byUnit].map(([unit, ws]) => ({ key: `unit:${unit}`, unit: unit || null, watches: ws }));
}

function CursorTime({ readout }: { readout: Readout }) {
  useReadout(readout, 1000);
  return (
    <span className="min-w-[9ch] font-mono text-[12px] text-muted tabular-nums">
      {readout.cursorT === null ? "" : `t = ${readout.cursorT.toFixed(3)} s`}
    </span>
  );
}

interface Props extends LegendActions {
  watches: Watch[];
  connected: boolean;
  /** The core is stopped, so values hold still */
  halted: boolean;
  onToggleSide: () => void;
}

export function Scope({ watches, connected, halted, onToggleSide, ...actions }: Props) {
  const [settings, setSettings] = useState(loadSettings);
  const [paused, setPausedState] = useState(false);
  const [theme, setTheme] = useState(0);
  const [readout] = useState<Readout>(newReadout);
  const recorder = useRecorder();
  const [optionsOpen, setOptionsOpen] = useState(false);
  const optionsAnchor = useRef<HTMLButtonElement>(null);
  const closeOptions = useCallback(() => setOptionsOpen(false), []);
  // The x window: follows the newest sample while live, holds still (or zooms) while paused
  const view = useRef({ windowSec: settings.windowSec, paused: false, from: 0, to: settings.windowSec, dirty: true });
  const hosts = useRef(new Map<string, HTMLDivElement>());
  const plots = useRef<{ lane: Lane; u: uPlot }[]>([]);
  const syncKey = useId();

  const lanes = useMemo(() => laneGroups(watches, settings.layout), [watches, settings.layout]);
  const structure = lanes.map((l) => `${l.key}=${l.watches.map((w) => `${w.id}:${w.trace}`).join(",")}`).join("|");

  const update = (patch: Partial<Settings>) => {
    const next = { ...settings, ...patch };
    setSettings(next);
    try {
      host.storage.set(SETTINGS_KEY, JSON.stringify(next));
    } catch {
      // Not remembered next launch; nothing else depends on it
    }
  };

  const setPaused = useCallback((p: boolean) => {
    view.current.paused = p;
    view.current.dirty = true;
    setPausedState(p);
  }, []);

  // Theme changes rebuild the canvases with the new colours
  useEffect(() => {
    const bump = () => setTheme((t) => t + 1);
    const media = matchMedia("(prefers-color-scheme: dark)");
    media.addEventListener("change", bump);
    const attrs = new MutationObserver(bump);
    attrs.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme", "data-host"] });
    return () => {
      media.removeEventListener("change", bump);
      attrs.disconnect();
    };
  }, []);

  // Space pauses and resumes, unless typing somewhere
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== " " || e.target !== document.body) return;
      e.preventDefault();
      setPaused(!view.current.paused);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [setPaused]);

  // One uPlot per lane, rebuilt when the lanes, their traces or the theme change
  useEffect(() => {
    const axisFont = `11px ${cssVar("--font-code") || "monospace"}`;
    const axis = {
      stroke: cssVar("--muted"),
      grid: { stroke: cssVar("--grid"), width: 1 },
      ticks: { stroke: cssVar("--grid"), width: 1, size: 4 },
      font: axisFont,
      gap: 3,
    };
    const built = lanes.map((lane, i) => {
      const el = hosts.current.get(lane.key)!;
      const last = i === lanes.length - 1;
      const onCursor = (u: uPlot) => {
        const idx = u.cursor.idx;
        if (idx === null || idx === undefined || (u.cursor.left ?? -1) < 0) {
          readout.cursorT = null;
          readout.cursor.clear();
        } else {
          // Every synced lane reports; each fills in its own traces
          readout.cursorT = u.data[0][idx];
          lane.watches.forEach((w, s) => readout.cursor.set(w.id, u.data[s + 1][idx] ?? null));
        }
        notify(readout);
      };
      const onSelect = (u: uPlot) => {
        if (u.select.width < 4) return;
        const from = u.posToVal(u.select.left, "x");
        const to = u.posToVal(u.select.left + u.select.width, "x");
        u.setSelect({ left: 0, top: 0, width: 0, height: 0 }, false);
        view.current.from = from;
        view.current.to = to;
        setPaused(true);
      };
      const opts: uPlot.Options = {
        width: Math.max(el.clientWidth, 10),
        height: Math.max(el.clientHeight, 10),
        legend: { show: false },
        padding: [10, 10, last ? 0 : 4, 0],
        cursor: {
          sync: { key: syncKey },
          drag: { x: true, y: false, setScale: false },
          points: { size: 6, fill: cssVar("--plot") },
        },
        scales: {
          // The window is set on every draw; see the frame loop
          x: { time: false, auto: false },
          y: {
            auto: true,
            range: (_u, min, max) => {
              if (min === null || max === null) return [-1, 1];
              // A value that barely moves keeps a readable span instead of magnifying float noise
              const span = Math.max(max - min, Math.abs(max + min) * 0.01, 1e-6);
              const mid = (max + min) / 2;
              return [mid - span * 0.6, mid + span * 0.6];
            },
          },
        },
        axes: [
          {
            ...axis,
            size: last ? 26 : 0,
            values: last ? (_u, ticks) => ticks.map((t) => `${+t.toFixed(3)} s`) : () => [],
          },
          { ...axis, size: 58, values: (_u, ticks, _axis, _space, step) => formatTicks(ticks, step) },
        ],
        series: [
          {},
          ...lane.watches.map((w) => ({
            label: w.path,
            stroke: cssVar(`--trace-${(w.trace ?? 0) + 1}`),
            width: 1.5,
            points: { show: false },
            spanGaps: false,
          })),
        ],
        hooks: { setCursor: [onCursor], setSelect: [onSelect] },
      };
      const u = new uPlot(opts, [[], ...lane.watches.map(() => [])], el);
      u.over.addEventListener("dblclick", () => setPaused(false));
      const resize = new ResizeObserver(() => {
        u.setSize({ width: Math.max(el.clientWidth, 10), height: Math.max(el.clientHeight, 10) });
        view.current.dirty = true;
      });
      resize.observe(el);
      return { lane, u, resize };
    });
    plots.current = built;
    view.current.dirty = true;
    return () => {
      built.forEach(({ u, resize }) => {
        resize.disconnect();
        u.destroy();
      });
      plots.current = [];
      readout.cursorT = null;
      readout.cursor.clear();
    };
    // `structure` stands for `lanes`
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [structure, theme, syncKey, readout, setPaused]);

  // Draw on animation frames when samples arrived or the view changed. uPlot evaluates a
  // scale's `range` only when it is first set, so the window is set explicitly on every draw;
  // leaving it to `setData` freezes the x-axis and new samples slide off its right edge.
  useEffect(() => {
    let raf = 0;
    let drawn = -1;
    const tick = () => {
      raf = requestAnimationFrame(tick);
      const v = view.current;
      if (!v.paused) {
        if (samples.version === drawn && !v.dirty) return;
        v.to = Math.max(samples.latestTime, v.windowSec);
        v.from = v.to - v.windowSec;
      } else if (!v.dirty) return;
      v.dirty = false;
      drawn = samples.version;
      for (const { lane, u } of plots.current) {
        const ids = lane.watches.map((w) => w.id);
        const shaped = samples.window(ids, v.from, v.to, Math.max(50, Math.floor(u.bbox.width / devicePixelRatio)));
        ids.forEach((id, s) => readout.ranges.set(id, shaped.ranges[s]));
        u.batch(() => {
          u.setData(shaped.data as uPlot.AlignedData, false);
          u.setScale("x", { min: v.from, max: v.to });
        });
      }
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [readout]);

  const setWindow = (windowSec: number) => {
    update({ windowSec });
    view.current.windowSec = windowSec;
    setPaused(false);
  };

  const chip = halted
    ? { text: "Target halted", dot: "rounded-full bg-danger" }
    : paused
      ? { text: "Chart paused", dot: "rounded-[1px] bg-warn" }
      : connected
        ? { text: "Live", dot: "rounded-full bg-good" }
        : { text: "Not connected", dot: "rounded-full bg-faint" };

  return (
    <section aria-label="Scope" className="grid min-h-0 grid-rows-[auto_auto_minmax(0,1fr)] bg-surface">
      <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5 border-b border-rule px-3 py-1">
        <h2 className="text-[13px] font-semibold">Scope</h2>
        <span className="inline-flex items-center gap-1.5 rounded-full border border-rule bg-panel pr-2.5 pl-2 text-[12px] leading-5">
          <span aria-hidden className={`h-[7px] w-[7px] ${chip.dot}`} />
          {chip.text}
        </span>
        <label className="flex items-center gap-1.5 text-muted">
          Window
          <select aria-label="Chart time window" className={field} value={settings.windowSec} onChange={(e) => setWindow(Number(e.target.value))}>
            {WINDOWS.map((seconds) => <option key={seconds} value={seconds}>{seconds} s</option>)}
          </select>
        </label>
        <button ref={optionsAnchor} className={ghostButton} onClick={() => setOptionsOpen((v) => !v)} aria-expanded={optionsOpen} aria-haspopup="dialog">Chart options…</button>
        <Popover anchor={optionsAnchor} open={optionsOpen} onClose={closeOptions} place="below-left" label="Chart options">
          <div className="grid gap-3 p-2">
            <span className="font-semibold">Group traces</span>
            <Segmented label="Layout" value={settings.layout} onChange={(layout) => update({ layout })} options={[
              { value: "lanes", label: "By unit", title: "Each unit has its own axis" },
              { value: "overlay", label: "Overlay", title: "All traces share one axis" },
            ]} />
            <p className="max-w-64 text-muted">Drag across a chart to zoom. Double-click to return to live. Space pauses or resumes the chart.</p>
          </div>
        </Popover>
        <RecordButton rec={recorder} connected={connected} />
        <span className="flex-1" />
        <CursorTime readout={readout} />
        <button
          onClick={() => setPaused(!paused)}
          title={paused ? "Double-clicking the plot also returns to live" : "Freeze the plot; drag across a lane to zoom"}
          className={`${button} text-[12px]`}
        >
          {paused ? "Resume chart" : "Pause chart"}
        </button>
      </div>
      <div>
        <RecordNotice rec={recorder} />
      </div>
      <div className="grid min-h-0 grid-cols-1 grid-rows-[minmax(0,1fr)_minmax(110px,25%)]">
        <div
          className="relative flex min-h-0 min-w-0 flex-col bg-plot"
          onPointerLeave={() => {
            readout.cursorT = null;
            readout.cursor.clear();
            notify(readout);
          }}
        >
          {lanes.map((lane) => (
            <div key={lane.key} className="relative min-h-0 flex-1 basis-0 border-b border-grid last:border-b-0">
              <span className="pointer-events-none absolute top-1 left-[62px] z-[2] rounded-sm bg-plot/85 px-1 text-[11px] text-muted">
                {lane.unit ?? (settings.layout === "overlay" ? "all traces" : "no unit")}
              </span>
              <div
                ref={(el) => {
                  if (el) hosts.current.set(lane.key, el);
                  else hosts.current.delete(lane.key);
                }}
                className="absolute inset-0"
              />
            </div>
          ))}
          {halted && lanes.length > 0 && (
            <div className="absolute top-2.5 left-1/2 z-[5] -translate-x-1/2 rounded-sm border border-danger bg-surface px-2.5 py-1 text-[12px] shadow-md">
              The core is halted. Values hold until it runs again.
            </div>
          )}
          {lanes.length === 0 && (
            <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 p-6 text-center">
              <p className="font-medium">{watches.length ? "Choose a value to plot" : "Add your first variable"}</p>
              <p className="max-w-sm text-muted">{watches.length ? "Click a value’s color swatch below to show it on the chart." : host.name === "vscode" ? "Use + beside a symbol in the workbench, or open Tune to watch a tuning value." : "Choose a symbol or tuning value from Variables to start plotting."}</p>
              {!watches.length && <button className={button} onClick={onToggleSide}>{host.name === "vscode" ? "Show tuning values" : "Show variables"}</button>}
            </div>
          )}
        </div>
        <Legend lanes={lanes} watches={watches} overlay={settings.layout === "overlay"} readout={readout} {...actions} />
      </div>
    </section>
  );
}
