import { useEffect, useRef, useState } from "react";
import type { Task } from "../elf/api";
import { host } from "../host";
import { button, field as input } from "../ui";
import { emptyTrace, ingestTrace, TraceModel } from "./traceModel";

export function TaskTimeline({ tasks, live, select }: { tasks: Task[]; live: boolean; select: (path: string) => void }) {
  const [model, setModel] = useState<TraceModel>(emptyTrace);
  const latest = useRef(model);
  const [hz, setHz] = useState(0);
  const [supported, setSupported] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [paused, setPaused] = useState(false);
  const [windowMs, setWindowMs] = useState(5000);
  const [longMs, setLongMs] = useState(2);
  const [visible, setVisible] = useState(!document.hidden);
  useEffect(() => { const change = () => setVisible(!document.hidden); document.addEventListener("visibilitychange", change); return () => document.removeEventListener("visibilitychange", change); }, []);
  useEffect(() => { latest.current = emptyTrace(); setModel(latest.current); setSupported(null); setHz(0); }, [tasks, live]);
  useEffect(() => {
    if (!live || paused || !visible) return;
    let stopped = false; let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const snapshot = await host.taskTrace();
        if (stopped) return;
        setSupported(snapshot !== null); setError(null);
        if (snapshot?.clockHz) { setHz(snapshot.clockHz); latest.current = ingestTrace(latest.current, snapshot); setModel(latest.current); }
      } catch (e) { if (!stopped) setError(String(e)); }
      finally { if (!stopped) timer = setTimeout(poll, 500); }
    };
    void poll(); return () => { stopped = true; clearTimeout(timer); };
  }, [live, paused, visible, tasks]);
  const end = hz ? model.ticks / hz * 1000 : 0;
  const start = end - windowMs;
  return <section className="flex min-h-0 flex-1 flex-col" aria-label="Task execution timeline">
    <div className="flex flex-wrap items-center gap-2 border-b border-rule px-3 py-2">
      <span className="text-muted">Recorded polls · {paused ? "frozen" : "live"}</span>
      <select className={input} aria-label="Timeline window" value={windowMs} onChange={(e) => setWindowMs(Number(e.target.value))}>{[1000, 5000, 10000, 30000].map((ms) => <option key={ms} value={ms}>{ms / 1000} s</option>)}</select>
      <label className="flex items-center gap-1 text-muted">Long poll above <input className={`${input} w-16`} aria-label="Long poll threshold in milliseconds" type="number" min="0.01" step="0.1" value={longMs} onChange={(e) => setLongMs(Math.max(0.01, Number(e.target.value) || 2))} /> ms</label>
      <button className={`${button} ml-auto`} onClick={() => setPaused((p) => !p)} disabled={!live}>{paused ? "Resume" : "Freeze"}</button>
    </div>
    {!live ? <p className="p-3 text-muted">Connect an SWD probe to read the task event buffer.</p> : supported === false ? <p className="p-3 text-muted">This firmware has no STUDIO_TASK_TRACE buffer. Add studio-task-trace and its Embassy hooks to enable the execution timeline. The Tasks view still provides sampled states.</p> : supported === null ? <p className="p-3 text-muted">Looking for the firmware event buffer…</p> : null}
    {supported && !hz && <p className="px-3 text-muted">The firmware has not initialized its trace clock yet.</p>}
    {error && <p role="alert" className="px-3 text-danger">Trace read failed; displayed events are stale: {error}</p>}
    {model.lost > 0 && <p role="status" className="px-3 text-warn">{model.lost} events missed or overwritten. Gaps are excluded from poll and latency measurements.</p>}
    {supported && <div className="min-h-0 flex-1 overflow-auto p-3">
      <div className="mb-1 flex justify-between pl-40 text-muted"><span>−{windowMs / 1000} s</span><span>Latest event</span></div>
      {tasks.map((task) => {
        const spans = model.spans.filter((s) => s.task === task.root.address && s.end >= start);
        const maximum = Math.max(0, ...spans.map((s) => s.end - s.start));
        const latencies = spans.flatMap((s) => s.latency === null ? [] : [s.latency]);
        const latency = latencies.length ? Math.max(...latencies).toFixed(3) + " ms" : "—";
        return <div key={task.root.path} className="flex min-w-[500px] items-center border-b border-grid py-1">
          <button className="w-40 shrink-0 truncate pr-2 text-left font-mono hover:text-accent" title={task.root.path} onClick={() => select(task.root.path)}>{task.root.label}<span className="block text-[10px] text-muted">max {maximum.toFixed(3)} ms · ready {latency}</span></button>
          <svg role="img" aria-label={`${task.root.label}: ${spans.length} completed polls, longest ${maximum.toFixed(3)} ms, maximum wake latency ${latency}`} viewBox="0 0 1000 30" preserveAspectRatio="none" className="h-10 min-w-0 flex-1 bg-panel">
            {[0, 250, 500, 750, 1000].map((x) => <line key={x} x1={x} x2={x} y1={0} y2={30} className="stroke-grid" />)}
            {spans.map((span, i) => <rect key={i} x={Math.max(0, (span.start - start) / windowMs * 1000)} y={5} height={20} width={Math.max(1, (span.end - Math.max(start, span.start)) / windowMs * 1000)} className={span.end - span.start >= longMs ? "fill-warn" : "fill-muted"}><title>{`Poll ${(span.end - span.start).toFixed(3)} ms; wake-to-run ${span.latency === null ? "not captured" : `${span.latency.toFixed(3)} ms`}`}</title></rect>)}
          </svg>
        </div>;
      })}
      <p className="mt-2 text-[11px] text-muted">Each bar is a completed poll; amber exceeds the threshold. Hover for duration and wake-to-run latency. Subpixel polls have a one-pixel marker. Idle time and missing events are not inferred.</p>
    </div>}
  </section>;
}
