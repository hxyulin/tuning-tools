import { test } from "node:test";
import assert from "node:assert/strict";
import { LatestValueWriter } from "../src/live/latestValueWriter.ts";

const drain = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };

test("drag writes are throttled, coalesced and final release is delivered", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout", "Date"], now: 1000 });
  const values: number[] = [];
  const writer = new LatestValueWriter(async (value) => { values.push(value); });
  writer.request(1);
  await drain();
  writer.request(2);
  writer.request(3);
  t.mock.timers.tick(99);
  await drain();
  assert.deepEqual(values, [1]);
  t.mock.timers.tick(1);
  await drain();
  assert.deepEqual(values, [1, 3]);
  writer.request(4);
  writer.request(5);
  writer.flush();
  await drain();
  assert.deepEqual(values, [1, 3, 5]);
  writer.cancel();
});

test("slow writes never overlap, cancellation discards queued values", async () => {
  const values: number[] = [];
  let finish: (() => void) | undefined;
  const writer = new LatestValueWriter((value) => { values.push(value); return new Promise<void>((resolve) => { finish = resolve; }); });
  writer.request(1);
  await drain();
  writer.request(2);
  writer.request(3);
  writer.flush();
  assert.deepEqual(values, [1]);
  finish!();
  await drain();
  assert.deepEqual(values, [1, 3]);
  writer.request(4);
  writer.cancel();
  finish!();
  await drain();
  assert.deepEqual(values, [1, 3]);
});

test("a rejected write stops queued values without an unhandled rejection", async () => {
  const values: number[] = [];
  const writer = new LatestValueWriter(async (value) => { values.push(value); throw new Error("lease lost"); });
  writer.request(1);
  writer.request(2);
  writer.flush();
  await drain();
  assert.deepEqual(values, [1]);
  writer.cancel();
});


test("cancelling before dispatch prevents a stale write after disconnect", async () => {
  const values: number[] = [];
  const writer = new LatestValueWriter(async (value) => { values.push(value); });
  writer.request(1);
  writer.cancel();
  await drain();
  assert.deepEqual(values, []);
});
