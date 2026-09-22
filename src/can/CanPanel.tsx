import { useEffect, useRef, useState } from "react";
import { host } from "../host";
import { button, field, primaryButton } from "../ui";
import type { CanFrame, CanRequest, CanSnapshot, ChannelConfig, Timing } from "./types";

const initialConfig: ChannelConfig = { channel: 0, fd: false, arbitrationBitrate: 1000000, dataBitrate: 2000000, arbitrationSamplePoint: 0.75, dataSamplePoint: 0.75, arbitrationTiming: null, dataTiming: null };
const timing: Timing = { seg1: 13, seg2: 2, sjw: 1, prescaler: 4 };
const empty: CanSnapshot = { connected: false, devices: [], configs: [], frames: [], sequence: 0, received: 0, transmitted: 0, errors: 0, dropped: 0, skipped: 0, recording: null, recorded: 0, error: null };
const hex = (n: number) => n.toString(16).toUpperCase();

function TimingFields({ title, value, change }: { title: string; value: Timing; change: (v: Timing) => void }) {
  return <fieldset className="flex flex-wrap items-center gap-2"><legend className="text-muted">{title}</legend>{(["seg1", "seg2", "sjw", "prescaler"] as const).map(key => <label key={key}>{key} <input className={`${field} w-16`} type="number" min={1} max={255} value={value[key]} onChange={e => change({ ...value, [key]: Number(e.target.value) })} /></label>)}</fieldset>;
}

export function CanPanel() {
  const [state, setState] = useState<CanSnapshot>(empty);
  const [frames, setFrames] = useState<CanFrame[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [library, setLibrary] = useState(() => host.storage.get("can-library") ?? "");
  const [index, setIndex] = useState(0);
  const [channels, setChannels] = useState(1);
  const [configs, setConfigs] = useState<ChannelConfig[]>([initialConfig]);
  const [filter, setFilter] = useState("");
  const [direction, setDirection] = useState("all");
  const [paused, setPaused] = useState(false);
  const [skipped, setSkipped] = useState(0);
  const cursor = useRef(0);
  const pauseRef = useRef(false);
  const [txChannel, setTxChannel] = useState(0);
  const [txId, setTxId] = useState("123");
  const [payload, setPayload] = useState("00 01 02 03");
  const [extended, setExtended] = useState(false);
  const [fd, setFd] = useState(false);
  const [brs, setBrs] = useState(false);
  const [rtr, setRtr] = useState(false);

  useEffect(() => {
    if (!host.canRequest) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const next = await host.canRequest!({ action: "poll", after: cursor.current });
        if (stopped) return;
        setState(next);
        if (!pauseRef.current) {
          cursor.current = next.sequence;
          setSkipped(n => n + next.skipped);
          if (next.frames.length) setFrames(old => [...old, ...next.frames].slice(-2000));
        }
      } catch (e) { if (!stopped) setError(String(e)); }
      if (!stopped) timer = setTimeout(poll, 100);
    }
    void poll();
    return () => { stopped = true; clearTimeout(timer); };
  }, []);

  async function act(request: CanRequest) {
    if (!host.canRequest) return;
    setBusy(true); setError(null);
    try {
      const next = await host.canRequest(request);
      setState(next);
      if (request.action === "connect") {
        setFrames([]); setSkipped(0); cursor.current = next.sequence;
        setTxChannel(request.configs[0].channel);
      }
      if (request.action === "scan") {
        host.storage.set("can-library", library);
        setIndex(next.devices[0]?.index ?? 0);
      }
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  async function record() {
    try {
      const path = await host.pickRecordingPath();
      if (path) await act({ action: "record", path });
    } catch (e) { setError(String(e)); }
  }
  async function recordCsv() {
    try {
      const path = await host.pickCsvPath(`can-${new Date().toISOString().replace(/[:.]/g, "-")}.csv`);
      if (path) await act({ action: "record", path });
    } catch (e) { setError(String(e)); }
  }
  function send() {
    if (!/^(?:0x)?[0-9a-f]+$/i.test(txId.trim())) { setError("Enter a hexadecimal CAN ID"); return; }
    const tokens = payload.trim() ? payload.trim().split(/\s+/) : [];
    if (tokens.some(t => !/^[0-9a-f]{2}$/i.test(t))) { setError("Enter payload bytes as two hex digits separated by spaces"); return; }
    void act({ action: "send", frame: { channel: txChannel, id: parseInt(txId, 16), extended, fd, brs, rtr, data: tokens.map(t => parseInt(t, 16)) } });
  }
  const filterText = filter.trim().replace(/^0x/i, "");
  const validFilter = !filterText || /^[0-9a-f]+$/i.test(filterText);
  const visible = frames.filter(f => (!filterText || (validFilter && f.id === parseInt(filterText, 16))) && (direction === "all" || f.direction === direction));
  if (!host.canRequest) return <div className="p-6 text-muted">CAN hardware is available in the desktop app and VS Code extension.</div>;

  return <section className="flex min-h-0 flex-1 flex-col overflow-auto bg-surface" aria-label="CAN bus">
    <div className="flex flex-wrap items-center gap-3 border-b border-rule p-3">
      <strong>CAN bus</strong><span className={state.connected ? "text-good" : "text-muted"}>{state.connected ? "Connected" : "Disconnected"}</span>
      <span className="text-muted">RX {state.received} · TX {state.transmitted} · Errors {state.errors} · Capture drops {state.dropped}</span>
      <span className="flex-1" />
      {state.connected ? <button className={button} disabled={busy} onClick={() => void act({ action: "disconnect" })}>Disconnect</button> : null}
    </div>
    {(error || state.error) && <div role="alert" className="px-3 py-2 text-danger">{error ?? state.error}</div>}
    <details open={!state.connected} className="border-b border-rule p-3">
      <summary className="cursor-pointer font-medium">Adapter & bus settings</summary>
      <fieldset disabled={busy || state.connected} className="mt-3 space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <label>SDK library <input className={`${field} w-80`} placeholder="Bundled SDK or absolute library path" value={library} onChange={e => setLibrary(e.target.value)} /></label>
          <button className={button} onClick={() => void act({ action: "scan", library: library.trim() || null })}>Scan adapters</button>
          <label>Adapter <select className={field} value={index} onChange={e => setIndex(Number(e.target.value))}>{state.devices.length ? state.devices.map(d => <option key={d.index} value={d.index}>{d.model} #{d.index}{d.version ? ` · ${d.version}` : ""}</option>) : <option>No adapters discovered</option>}</select></label>
          <label>Model <select className={field} value={channels} onChange={e => { const n = Number(e.target.value); setChannels(n); setConfigs([initialConfig]); setTxChannel(0); }}><option value={1}>USB2CANFD</option><option value={2}>USB2CANFD_DUAL</option><option value={4}>LinkX4C</option></select></label>
        </div>
        {Array.from({ length: channels }, (_, ch) => {
          const config = configs.find(c => c.channel === ch);
          function update(patch: Partial<ChannelConfig>) { setConfigs(cs => cs.map(c => c.channel === ch ? { ...c, ...patch } : c)); }
          return <div className="rounded-sm border border-rule p-2" key={ch}>
            <label className="font-medium"><input type="checkbox" checked={!!config} onChange={e => setConfigs(cs => e.target.checked ? [...cs, { ...initialConfig, channel: ch }] : cs.filter(c => c.channel !== ch))} /> Channel {ch}</label>
            {config && <div className="mt-2 space-y-2">
              <div className="flex flex-wrap items-center gap-3">
                <label><input type="checkbox" checked={config.fd} onChange={e => update({ fd: e.target.checked, dataTiming: e.target.checked && config.arbitrationTiming ? timing : null })} /> CAN FD</label>
                <label>Arbitration bitrate (bit/s) <input className={`${field} w-28`} type="number" min={1} disabled={!!config.arbitrationTiming} value={config.arbitrationBitrate} onChange={e => update({ arbitrationBitrate: Number(e.target.value) })} /></label>
                <label>Sample point <input className={`${field} w-20`} type="number" min={0.01} max={0.99} step={0.01} disabled={!!config.arbitrationTiming} value={config.arbitrationSamplePoint} onChange={e => update({ arbitrationSamplePoint: Number(e.target.value) })} /></label>
                {config.fd && <><label>FD data bitrate (bit/s) <input className={`${field} w-28`} type="number" min={1} disabled={!!config.arbitrationTiming} value={config.dataBitrate} onChange={e => update({ dataBitrate: Number(e.target.value) })} /></label><label>Data sample point <input className={`${field} w-20`} type="number" min={0.01} max={0.99} step={0.01} disabled={!!config.arbitrationTiming} value={config.dataSamplePoint} onChange={e => update({ dataSamplePoint: Number(e.target.value) })} /></label></>}
              </div>
              <label><input type="checkbox" checked={!!config.arbitrationTiming} onChange={e => update({ arbitrationTiming: e.target.checked ? timing : null, dataTiming: e.target.checked && config.fd ? timing : null })} /> Advanced register timing</label>
              {config.arbitrationTiming && <><TimingFields title="Arbitration timing (SDK register values)" value={config.arbitrationTiming} change={v => update({ arbitrationTiming: v })} />{config.fd && config.dataTiming && <TimingFields title="Data timing (SDK register values)" value={config.dataTiming} change={v => update({ dataTiming: v })} />}<p className="text-muted">Register timing overrides bitrate/sample-point fields. Values depend on the adapter clock.</p></>}
            </div>}
          </div>;
        })}
        <p className="text-muted">Match the bus settings before connecting. The SDK does not expose a silent/listen-only mode; the adapter may acknowledge bus traffic. Disconnect to change settings.</p>
        <button className={primaryButton} disabled={!state.devices.length || !configs.length} onClick={() => void act({ action: "connect", index, channels, configs })}>Connect CAN</button>
      </fieldset>
    </details>
    <div className="flex flex-wrap items-center gap-3 border-b border-rule p-3">
      <label>Filter ID (hex) <input className={`${field} w-28`} value={filter} aria-invalid={!validFilter} onChange={e => setFilter(e.target.value)} placeholder="All IDs" /></label>
      <select aria-label="Frame direction" className={field} value={direction} onChange={e => setDirection(e.target.value)}><option value="all">All directions</option><option value="rx">RX</option><option value="tx">TX</option><option value="error">Errors</option></select>
      <button className={button} onClick={() => { pauseRef.current = !paused; setPaused(!paused); }}>{paused ? "Resume view" : "Pause view"}</button>
      <button className={button} onClick={() => setFrames([])}>Clear view</button>
      <span className="text-muted">Latest 2,000 frames · {skipped} skipped by view</span>
      <span className="flex-1" />
      {state.recording ? <><span className="text-muted" title={state.recording}>Recording · {state.recorded} frames</span><button className={button} disabled={busy} onClick={() => void act({ action: "stopRecording" })}>Stop recording</button></> : <><button className={button} disabled={busy || !state.connected} onClick={() => void record()}>Record MCAP…</button><button className={button} disabled={busy || !state.connected} onClick={() => void recordCsv()}>Record CSV…</button></>}
    </div>
    <div className="min-h-[180px] flex-1 overflow-auto">
      <table className="w-full whitespace-nowrap text-left font-mono text-xs"><thead className="sticky top-0 bg-panel text-muted"><tr>{["#", "Device timestamp (raw)", "CH", "Dir", "ID", "Flags", "DLC", "Data (hex)"].map(s => <th className="px-3 py-2" key={s}>{s}</th>)}</tr></thead><tbody>{visible.map(f => <tr className="border-t border-rule" key={f.sequence}><td className="px-3 py-1">{f.sequence}</td><td className="px-3" title={`Host Unix ns: ${f.hostTimestampNs}`}>{f.deviceTimestamp}</td><td className="px-3">{f.channel}</td><td className="px-3">{f.direction.toUpperCase()}</td><td className="px-3">{hex(f.id).padStart(f.extended ? 8 : 3, "0")}</td><td className="px-3">{[f.extended ? "EXT" : "STD", f.fd && "FD", f.brs && "BRS", f.rtr && "RTR", f.esi && "ESI", f.ack && "ACK"].filter(Boolean).join(" ")}</td><td className="px-3">{f.dlc} ({f.data.length})</td><td className="px-3">{f.data.map(b => hex(b).padStart(2, "0")).join(" ")}</td></tr>)}</tbody></table>
      {!visible.length && <p className="p-6 text-muted">{state.connected ? "Waiting for matching CAN frames…" : "Scan an adapter and connect to start capturing. No firmware ELF is needed."}</p>}
    </div>
    <fieldset disabled={busy || !state.connected} className="flex flex-wrap items-center gap-3 border-t border-rule p-3">
      <legend className="px-2 font-medium">Transmit one frame</legend>
      <label>Channel <select className={field} value={txChannel} onChange={e => setTxChannel(Number(e.target.value))}>{(state.connected ? state.configs : configs).map(c => <option key={c.channel} value={c.channel}>{c.channel}</option>)}</select></label>
      <label>ID (hex) <input className={`${field} w-24`} value={txId} onChange={e => setTxId(e.target.value)} /></label>
      <label>Data (hex bytes) <input className={`${field} w-64 font-mono`} value={payload} onChange={e => setPayload(e.target.value)} placeholder="01 02 AB CD" /></label>
      <label><input type="checkbox" checked={extended} onChange={e => setExtended(e.target.checked)} /> Extended</label>
      <label><input type="checkbox" checked={fd} onChange={e => { setFd(e.target.checked); if (!e.target.checked) setBrs(false); if (e.target.checked) setRtr(false); }} /> FD</label>
      <label><input type="checkbox" disabled={!fd} checked={brs} onChange={e => setBrs(e.target.checked)} /> BRS</label>
      <label><input type="checkbox" disabled={fd} checked={rtr} onChange={e => { setRtr(e.target.checked); if (e.target.checked) setPayload(""); }} /> RTR</label>
      <button className={primaryButton} onClick={send}>Send once</button>
    </fieldset>
  </section>;
}
