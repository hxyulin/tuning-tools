import type { TraceSnapshot } from "./api";
export interface PollSpan { task: number; start: number; end: number; latency: number | null }
export interface TraceModel { head: number | null; clockHz: number; ticks: number; lost: number; starts: Map<number, number>; ready: Map<number, number>; latencies: Map<number, number>; spans: PollSpan[] }
export const emptyTrace = (): TraceModel => ({ head: null, clockHz: 0, ticks: 0, lost: 0, starts: new Map(), ready: new Map(), latencies: new Map(), spans: [] });
/** Reconstruct only complete polls. Never connect bars across missing records. */
export function ingestTrace(old: TraceModel, snapshot: TraceSnapshot): TraceModel {
  const newest = snapshot.events[snapshot.events.length - 1]?.ticks ?? old.ticks;
  const reset = old.clockHz !== 0 && old.clockHz !== snapshot.clockHz || old.head !== null && (((snapshot.head - old.head) >>> 0) > 0x80000000 || newest < old.ticks);
  const base = reset ? emptyTrace() : old;
  const next: TraceModel = { ...base, clockHz: snapshot.clockHz, starts: new Map(base.starts), ready: new Map(base.ready), latencies: new Map(base.latencies), spans: [...base.spans] };
  for (const event of snapshot.events) {
    if (next.head !== null) {
      const distance = (event.seq - next.head) >>> 0;
      if (distance === 0 || distance > 0x80000000) continue;
      if (distance > 1) { next.lost += distance - 1; next.starts.clear(); next.ready.clear(); next.latencies.clear(); }
    }
    const t = event.ticks / snapshot.clockHz * 1000;
    if (event.kind === 1 && !next.ready.has(event.task)) next.ready.set(event.task, t);
    if (event.kind === 2) {
      next.starts.set(event.task, t);
      const ready = next.ready.get(event.task);
      next.latencies.delete(event.task);
      if (ready !== undefined && ready <= t) next.latencies.set(event.task, t - ready);
      next.ready.delete(event.task);
    }
    if (event.kind === 3) {
      const start = next.starts.get(event.task);
      if (start !== undefined && t >= start) next.spans.push({ task: event.task, start, end: t, latency: next.latencies.get(event.task) ?? null });
      next.starts.delete(event.task); next.latencies.delete(event.task);
    }
    if (event.kind === 4) { next.ready.delete(event.task); }
    next.head = event.seq; next.ticks = event.ticks;
  }
  const end = next.ticks / snapshot.clockHz * 1000;
  next.spans = next.spans.filter((s) => s.end >= end - 30_000).slice(-20_000);
  return next;
}

/** Clamp navigation to the retained history, allowing empty space for a wider window. */
export function traceViewport(model: TraceModel, requestedEnd: number | null, width: number) {
  const latest = model.clockHz ? model.ticks / model.clockHz * 1000 : 0;
  const earliest = Math.max(0, latest - 30_000, model.spans.reduce((first, span) => Math.min(first, span.start), latest));
  const minimumEnd = Math.min(latest, earliest + width);
  const end = Math.max(minimumEnd, Math.min(latest, requestedEnd ?? latest));
  return { start: end - width, end, earliest };
}
