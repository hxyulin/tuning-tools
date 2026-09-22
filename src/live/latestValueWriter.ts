/** At most one write in flight; replace intermediate drag values with the latest one. */
export class LatestValueWriter {
  private pending: number | null = null;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private active = false;
  private stopped = false;
  private immediate = false;
  private lastStart = -Infinity;

  private send: (value: number) => Promise<void>;
  private intervalMs: number;
  constructor(send: (value: number) => Promise<void>, intervalMs = 100) {
    this.send = send;
    this.intervalMs = intervalMs;
  }

  request(value: number) {
    if (this.stopped) return;
    this.pending = value;
    this.pump();
  }

  flush() {
    this.immediate = true;
    clearTimeout(this.timer);
    this.timer = undefined;
    this.pump();
  }

  cancel() {
    this.stopped = true;
    this.pending = null;
    clearTimeout(this.timer);
  }

  private pump() {
    if (this.stopped || this.active || this.pending === null) return;
    const delay = this.immediate ? 0 : this.intervalMs - (Date.now() - this.lastStart);
    if (delay > 0) {
      if (this.timer === undefined) this.timer = setTimeout(() => { this.timer = undefined; this.pump(); }, delay);
      return;
    }
    clearTimeout(this.timer);
    this.timer = undefined;
    this.immediate = false;
    const value = this.pending;
    this.pending = null;
    this.active = true;
    this.lastStart = Date.now();
    // The caller owns error presentation. A failure stops this drag's pending writes.
    void Promise.resolve().then(() => { if (!this.stopped) return this.send(value); }).catch(() => { this.pending = null; }).finally(() => {
      this.active = false;
      this.pump();
    });
  }
}
