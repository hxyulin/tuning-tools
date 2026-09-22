import assert from "node:assert/strict";
import { build } from "esbuild";
import { runInNewContext } from "node:vm";
const {outputFiles} = await build({entryPoints:["../src/live/traceModel.ts"],bundle:true,write:false,platform:"node",format:"cjs"});
const context={module:{exports:{}},exports:{}};runInNewContext(outputFiles[0].text,context);
const {emptyTrace,ingestTrace,traceViewport}=context.module.exports;
const snap=(events)=>({clockHz:1000,head:events[events.length-1].seq,capacity:10,incomplete:0,events});
const e=(seq,ticks,kind,task=1)=>({seq,ticks,kind,task});
let model=ingestTrace(emptyTrace(),snap([e(1,10,1),e(2,15,2),e(3,20,3)]));
assert.equal(model.spans[0].latency,5);assert.equal(model.spans[0].end-model.spans[0].start,5);
model=ingestTrace(model,snap([e(2,15,2),e(3,20,3)]));assert.equal(model.spans.length,1,"overlapping reads deduplicate");
model=ingestTrace(model,snap([e(5,30,3)]));assert.equal(model.lost,1);assert.equal(model.spans.length,1,"no invented bar across missing start");
model=ingestTrace(model,snap([e(6,40,2),e(7,45,4),e(8,50,3)]));assert.equal(model.spans.length,2,"task exit precedes its final exec_end hook");
model=ingestTrace(model,snap([e(1,1,2),e(2,2,3)]));assert.equal(model.spans.length,1);assert.equal(model.lost,0,"reset clears old trace");
model=ingestTrace(emptyTrace(),snap([e(0xffffffff,100,2),e(0,105,3)]));assert.equal(model.spans.length,1,"32-bit sequence rollover");
const clockChange={...snap([e(1,200,2),e(2,210,3)]),clockHz:2000};model=ingestTrace(model,clockChange);assert.equal(model.spans[0].end-model.spans[0].start,5,"clock reinitialization clears old units");
console.log("task trace: latency, poll duration, deduplication, loss, task exit and reset pass");

const history = { ...emptyTrace(), clockHz: 1000, ticks: 50000, spans: [
  {task: 1, start: 24000, end: 25000, latency: null},
  {task: 2, start: 22000, end: 26000, latency: 1},
  {task: 1, start: 49000, end: 50000, latency: 2},
] };
assert.equal(traceViewport(history, null, 5000).end, 50000, "live follows latest");
assert.equal(traceViewport(history, 999999, 5000).end, 50000, "no navigation beyond latest");
assert.equal(traceViewport(history, 0, 5000).start, 22000, "earliest nested poll retained");
assert.equal(traceViewport(history, 30000, 1000).start, 29000, "zoom preserves requested range");
assert.equal(traceViewport(history, 0, 30000).end, 50000, "wide window stays bounded");
assert.equal(traceViewport(emptyTrace(), null, 5000).end, 0, "empty history is finite");
console.log("timeline viewport: live following, pan bounds, nested polls and zoom pass");
