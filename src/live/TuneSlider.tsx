import { useEffect, useRef, useState } from "react";
import type { CatalogEntry } from "../elf/api";
import { formatValue } from "./format";
import { LatestValueWriter } from "./latestValueWriter";
import { sliderRange } from "./tuningPresentation";

export function TuneSlider({ entry, value, disabled, onSend }: {
  entry: CatalogEntry;
  value: number | null;
  disabled: boolean;
  onSend: (value: number) => Promise<void>;
}) {
  const [draft, setDraft] = useState<number | null>(null);
  const send = useRef(onSend);
  send.current = onSend;
  const writer = useRef<LatestValueWriter | null>(null);
  useEffect(() => {
    setDraft(null);
    if (disabled) return;
    let mounted = true;
    const queue = new LatestValueWriter(async (next) => {
      try { await send.current(next); if (mounted) setDraft((current) => current === next ? null : current); }
      catch (error) { if (mounted) setDraft(null); throw error; }
    });
    writer.current = queue;
    return () => { mounted = false; queue.cancel(); writer.current = null; };
  }, [entry.id, disabled]);
  useEffect(() => {
    if (draft !== null && value !== null && (entry.kind === "f32" ? Math.fround(draft) === Math.fround(value) : draft === value)) setDraft(null);
  }, [draft, value, entry.kind]);
  const range = sliderRange(entry);
  if (!range) return null;
  const shown = Math.max(range.min, Math.min(range.max, draft ?? value ?? entry.default));
  const finish = () => { writer.current?.flush(); };
  return <input
    type="range"
    aria-label={`${entry.name} slider`}
    aria-valuetext={`${formatValue(shown, entry.kind)}${entry.unit ? ` ${entry.unit}` : ""}`}
    title={`${range.min} – ${range.max}${entry.unit ? ` ${entry.unit}` : ""}`}
    min={range.min} max={range.max} step={range.step} value={shown}
    disabled={disabled}
    onChange={(event) => {
      const next = Number(event.currentTarget.value);
      setDraft(next);
      writer.current?.request(next);
    }}
    onPointerUp={finish} onPointerCancel={finish} onKeyUp={finish} onBlur={finish}
    className="col-span-2 my-1 w-full accent-[var(--accent)] disabled:opacity-40"
  />;
}
