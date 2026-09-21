import { useCallback, useRef, useState } from "react";
import { MenuItem, Popover } from "../ui";
import { host } from "../host";
import { formatLive, formatTick } from "./format";
import { samples } from "./samples";
import { Readout, useReadout } from "./readout";
import type { Lane } from "./Scope";
import { MAX_TRACES, Watch, watchName } from "./useWatches";

/** Values change far faster than people read; repaint at 10 Hz, and on every cursor move */
const REFRESH_MS = 100;

export interface LegendActions {
  onTogglePlot: (id: number) => void;
  onRemove: (id: number) => void;
  onClear: () => void;
  onSetUnit: (id: number, unit: string | null) => void;
  /** The value can be written from the list: a tuning value, while the target runs this build */
  writable: (watch: Watch) => boolean;
  /** Rejects with the reason the value was not written */
  onWrite: (watch: Watch, value: number) => Promise<void>;
}

interface Props extends LegendActions {
  lanes: Lane[];
  watches: Watch[];
  overlay: boolean;
  readout: Readout;
}

function Head({ children, unit }: { children: React.ReactNode; unit?: string | null }) {
  return (
    <div className="sticky top-0 z-[1] flex items-baseline gap-1.5 bg-surface px-2.5 pt-[7px] pb-[3px] text-[10.5px] tracking-[.05em] text-muted uppercase">
      {children}
      {unit && <span className="ml-auto font-mono tracking-normal text-faint normal-case">{unit}</span>}
    </div>
  );
}

export function Legend({ lanes, watches, overlay, readout, ...actions }: Props) {
  useReadout(readout, REFRESH_MS);
  const plottedCount = watches.filter((w) => w.plotted).length;
  const inLanes = new Set(lanes.flatMap((l) => l.watches.map((w) => w.id)));
  const rest = watches.filter((w) => !inLanes.has(w.id));
  const hovering = readout.cursorT !== null;

  const row = (w: Watch) => (
    <Row
      key={w.id}
      watch={w}
      inLane={inLanes.has(w.id)}
      canPlot={w.plotted || plottedCount < MAX_TRACES}
      cursor={hovering && readout.cursor.has(w.id) ? { value: readout.cursor.get(w.id) ?? null } : null}
      range={readout.ranges.get(w.id) ?? null}
      {...actions}
    />
  );

  return (
    <div aria-label="Watched values" className="flex min-h-0 flex-col border-t border-rule bg-surface text-[12px]">
      <div className="min-h-0 flex-1 overflow-auto">
        <div className="px-2.5 pt-2 text-[11px] font-semibold uppercase tracking-wider text-muted">Watched values</div>
        {watches.length === 0 && (
          <p className="p-3 leading-relaxed text-muted">
            {host.name === "vscode" ? "Use + beside a symbol in the workbench, or watch a value in Tune." : "Pick a number in Symbols and press W, or watch a value in Tune."}
          </p>
        )}
        {lanes.map((lane, i) => (
          <section key={lane.key}>
            <Head unit={lane.unit ?? "no unit"}>{overlay ? "All traces" : `Lane ${i + 1}`}</Head>
            {lane.watches.map(row)}
          </section>
        ))}
        {rest.length > 0 && (
          <section>
            <Head>Watched, not plotted</Head>
            {rest.map(row)}
          </section>
        )}
      </div>
      {watches.length > 0 && (
        <div className="flex items-center gap-2 border-t border-rule px-2.5 py-1 text-[11px] text-muted tabular-nums">
          <span>
            {watches.length} {watches.length === 1 ? "value" : "values"}, {plottedCount} of {MAX_TRACES} plotted
          </span>
          <button onClick={actions.onClear} className="ml-auto rounded-sm px-1.5 hover:bg-sunken hover:text-ink">
            Remove all
          </button>
        </div>
      )}
    </div>
  );
}

interface RowProps extends LegendActions {
  watch: Watch;
  inLane: boolean;
  canPlot: boolean;
  /** The value under the cursor, while hovering a lane that plots it */
  cursor: { value: number | null } | null;
  range: [number, number] | null;
}

function Row({ watch: w, inLane, canPlot, cursor, range, onTogglePlot, onRemove, onSetUnit, writable, onWrite }: RowProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuAnchor = useRef<HTMLButtonElement>(null);
  const closeMenu = useCallback(() => setMenuOpen(false), []);
  const cut = w.path.lastIndexOf("::");
  const name = watchName(w.path);
  const module = w.cell !== null ? "tuning table" : cut < 0 ? "" : w.path.slice(0, cut);
  const latest = samples.latest(w.id);
  const value = cursor ? cursor.value : latest;
  const failed = !cursor && latest !== undefined && Number.isNaN(latest);

  return (
    <div onContextMenu={(e) => { e.preventDefault(); setMenuOpen(true); }} className="group grid grid-cols-[14px_minmax(0,1fr)_auto_18px] items-center gap-x-[7px] px-2.5 py-[3px] hover:bg-panel">
      <button
        onClick={() => onTogglePlot(w.id)}
        disabled={!canPlot}
        aria-pressed={w.plotted}
        title={w.plotted ? "Stop plotting" : canPlot ? "Plot" : `At most ${MAX_TRACES} traces`}
        className="h-3 w-3 rounded-[2px] border border-rule p-0 disabled:opacity-40"
        style={w.plotted && w.trace !== null ? { background: `var(--trace-${w.trace + 1})`, borderColor: "transparent" } : undefined}
      >
        <span className="sr-only">{w.plotted ? "Plotted" : "Not plotted"}</span>
      </button>
      <span className="truncate font-mono" title={`${w.path}\n${w.typeName}`}>
        {name}
      </span>
      {w.error ? (
        <span className="text-right text-danger" title={w.error}>
          cannot sample
        </span>
      ) : (
        <Value
          text={failed ? "read failed" : formatLive(value, w.scalar)}
          tone={failed ? "danger" : cursor && inLane ? "muted" : "ink"}
          current={latest}
          writable={writable(w)}
          onWrite={(v) => onWrite(w, v)}
        />
      )}
      <button
        ref={menuAnchor}
        onClick={() => setMenuOpen((v) => !v)}
        aria-label={`Options for ${w.path}`}
        aria-expanded={menuOpen}
        aria-haspopup="dialog"
        className="rounded-sm p-0 text-muted hover:bg-sunken hover:text-ink"
      >
        ⋯
      </button>
      <Popover anchor={menuAnchor} open={menuOpen} onClose={closeMenu} place="above-right" label={`Options for ${name}`}>
        <div className="max-w-72 break-words px-2 py-1 font-mono text-muted">{w.path}</div>
        <div role="menu">
          <MenuItem disabled={!canPlot} onSelect={() => { onTogglePlot(w.id); closeMenu(); }}>{w.plotted ? "Hide from chart" : "Show on chart"}</MenuItem>
          <MenuItem onSelect={() => { onRemove(w.id); closeMenu(); }}>Stop watching</MenuItem>
        </div>
      </Popover>
      <span className="col-start-2 col-end-4 flex min-w-0 justify-between gap-2 font-mono text-[10.5px] text-faint tabular-nums">
        <span className="truncate">
          {inLane ? (range ? `min ${formatTick(range[0])} · max ${formatTick(range[1])}` : "") : module}
        </span>
        <Unit unit={w.unit} onChange={(unit) => onSetUnit(w.id, unit)} />
      </span>
    </div>
  );
}

const toneClass = { ink: "", muted: "text-muted", danger: "text-danger" };

/** A reading; click it to write a new value when the value is writable */
function Value({
  text,
  tone,
  current,
  writable,
  onWrite,
}: {
  text: string;
  tone: keyof typeof toneClass;
  current: number | undefined;
  writable: boolean;
  onWrite: (value: number) => Promise<void>;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const shown = <span className={`text-right font-mono tabular-nums ${toneClass[tone]}`}>{text}</span>;

  if (!writable) return shown;
  if (draft === null) {
    return (
      <button
        onClick={() => {
          setDraft(current === undefined || Number.isNaN(current) ? "" : String(current));
          setError(null);
        }}
        title={error ?? "Write a new value"}
        className={`rounded-sm text-right font-mono tabular-nums underline decoration-faint decoration-dotted underline-offset-2 hover:bg-sunken ${
          error ? "text-danger" : toneClass[tone]
        }`}
      >
        {text}
      </button>
    );
  }
  const send = async () => {
    const value = Number(draft.trim());
    if (draft.trim() === "" || !Number.isFinite(value)) {
      setError("Enter a number.");
      return;
    }
    setSending(true);
    try {
      await onWrite(value);
      setDraft(null);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setSending(false);
    }
  };
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        void send();
      }}
    >
      <input
        autoFocus
        value={draft}
        disabled={sending}
        inputMode="decimal"
        spellCheck={false}
        aria-label="New value"
        title={error ?? "Enter to write, Escape to cancel"}
        onChange={(e) => setDraft(e.currentTarget.value)}
        onKeyDown={(e) => e.key === "Escape" && setDraft(null)}
        onBlur={() => !sending && setDraft(null)}
        className={`w-20 rounded-sm border bg-plot px-1 text-right font-mono ${error ? "border-danger" : "border-accent"}`}
      />
    </form>
  );
}

/** The unit that picks a value's lane; click to change it */
function Unit({ unit, onChange }: { unit: string | null; onChange: (unit: string | null) => void }) {
  const [draft, setDraft] = useState<string | null>(null);
  if (draft === null) {
    return (
      <button
        onClick={() => setDraft(unit ?? "")}
        title="Unit: traces with the same unit share a lane"
        className="shrink-0 rounded-sm px-0.5 hover:bg-sunken hover:text-ink"
      >
        {unit ?? "unit"}
      </button>
    );
  }
  const commit = () => {
    onChange(draft.trim() || null);
    setDraft(null);
  };
  return (
    <input
      autoFocus
      value={draft}
      spellCheck={false}
      aria-label="Unit"
      placeholder="unit"
      onChange={(e) => setDraft(e.currentTarget.value)}
      onKeyDown={(e) => {
        if (e.key === "Enter") commit();
        else if (e.key === "Escape") setDraft(null);
      }}
      onBlur={commit}
      className="w-14 rounded-sm border border-accent bg-plot px-1 text-right"
    />
  );
}
