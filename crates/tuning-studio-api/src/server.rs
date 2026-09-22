//! The firmware end of a framed link: answers requests against a [`Table`]
//! and produces sample frames for one subscription.
//!
//! It owns no transport and reads no clock. The caller feeds it decoded
//! frames with the time and whether the robot is safe, and sends what it
//! returns. One server serves one link.

use crate::wire::{cmd, Header, Overflow, Reader, Status, Writer, MAX_FRAME, REPLY};
use crate::{Access, Kind, Table, TABLE_VERSION};

/// A lease not renewed for this long lapses, and writes need it taken again.
pub const LEASE_MS: u16 = 3000;
pub const MAX_WATCHES: usize = 32;
/// Sample frame bytes per second one subscription may cost.
pub const SAMPLE_BUDGET_BYTES_PER_S: u32 = 64_000;

/// Per-request facts the server cannot know itself.
#[derive(Copy, Clone, Debug)]
pub struct Context {
    pub now_us: u64,
    /// The robot is in a state where `SafeOnly` values may change.
    pub safe: bool,
    /// Frames the link's decoder has dropped, reported by `STATS`.
    pub bad_frames: u32,
}

#[derive(Debug)]
pub struct Server {
    table: &'static Table,
    save_supported: bool,
    /// Token and expiry time.
    lease: Option<(u32, u64)>,
    watches: [u16; MAX_WATCHES],
    watch_len: usize,
    period_us: u64,
    next_sample_us: u64,
    sample_seq: u16,
    sent: u32,
    dropped: u32,
    /// Sequence number of the SAVE waiting on storage.
    save: Option<u16>,
}

impl From<Overflow> for Status {
    fn from(_: Overflow) -> Self {
        Self::BadFrame
    }
}

fn status_reply(reply: Header, status: Status, out: &mut [u8; MAX_FRAME]) -> usize {
    Writer::new(out, reply)
        .and_then(|mut w| {
            w.u8(status as u8)?;
            Ok(w.finish())
        })
        .unwrap_or(0)
}

fn done(r: &Reader<'_>) -> Result<(), Status> {
    if r.remaining() == 0 {
        Ok(())
    } else {
        Err(Status::BadFrame)
    }
}

impl Server {
    #[must_use]
    pub const fn new(table: &'static Table) -> Self {
        Self {
            table,
            save_supported: true,
            lease: None,
            watches: [0; MAX_WATCHES],
            watch_len: 0,
            period_us: 0,
            next_sample_us: 0,
            sample_seq: 0,
            sent: 0,
            dropped: 0,
            save: None,
        }
    }

    /// Serve volatile tuning without a persistence implementation.
    /// SAVE returns `SaveUnsupported` and never becomes pending.
    #[must_use]
    pub const fn without_storage(table: &'static Table) -> Self {
        let mut server = Self::new(table);
        server.save_supported = false;
        server
    }

    /// Answer one request into `out`; returns the reply's length.
    ///
    /// An accepted SAVE has no reply yet: the caller sees [`Self::save_pending`],
    /// writes [`crate::store`] and answers with [`Self::finish_save`].
    pub fn handle(
        &mut self,
        ctx: Context,
        header: Header,
        payload: &[u8],
        out: &mut [u8; MAX_FRAME],
    ) -> usize {
        let reply = Header {
            cmd: header.cmd | REPLY,
            flags: 0,
            seq: header.seq,
        };
        if header.cmd == cmd::SAVE {
            match self.begin_save(ctx, header.seq, payload) {
                Ok(()) => return 0,
                Err(status) => return status_reply(reply, status, out),
            }
        }
        let result = Writer::new(out, reply)
            .map_err(Status::from)
            .and_then(|mut w| {
                w.u8(Status::Ok as u8)?;
                self.body(ctx, header.cmd, Reader::new(payload), &mut w)?;
                Ok(w.finish())
            });
        match result {
            Ok(n) => n,
            Err(status) => status_reply(reply, status, out),
        }
    }

    fn begin_save(&mut self, ctx: Context, seq: u16, payload: &[u8]) -> Result<(), Status> {
        let mut r = Reader::new(payload);
        let token = r.u32().ok_or(Status::BadFrame)?;
        done(&r)?;
        self.leased(token, ctx.now_us)?;
        if !self.save_supported {
            return Err(Status::SaveUnsupported);
        }
        if self.save.is_some() {
            return Err(Status::Busy);
        }
        self.save = Some(seq);
        Ok(())
    }

    /// A SAVE is waiting for the caller to write storage.
    #[must_use]
    pub const fn save_pending(&self) -> bool {
        self.save.is_some()
    }

    /// Answer the pending SAVE with the saved generation or why it failed;
    /// returns the reply's length, 0 when none was pending.
    pub fn finish_save(&mut self, result: Result<u32, Status>, out: &mut [u8; MAX_FRAME]) -> usize {
        let Some(seq) = self.save.take() else {
            return 0;
        };
        let reply = Header {
            cmd: cmd::SAVE | REPLY,
            flags: 0,
            seq,
        };
        match result {
            Ok(generation) => Writer::new(out, reply)
                .and_then(|mut w| {
                    w.u8(Status::Ok as u8)?.u32(generation)?;
                    Ok(w.finish())
                })
                .unwrap_or(0),
            Err(status) => status_reply(reply, status, out),
        }
    }

    fn body(
        &mut self,
        ctx: Context,
        code: u8,
        mut r: Reader<'_>,
        w: &mut Writer<'_>,
    ) -> Result<(), Status> {
        let entries = self.table.entries();
        match code {
            cmd::HELLO => {
                done(&r)?;
                let (_, table_version) = self.table.format();
                w.u8(crate::wire::VERSION)?
                    .u32(table_version.min(TABLE_VERSION))?
                    .u16(entries.len() as u16)?
                    .u32(self.table.fingerprint())?
                    .u16(crate::wire::MAX_PAYLOAD as u16)?
                    .u16(LEASE_MS)?;
            }
            cmd::LEASE => {
                let token = r.u32().ok_or(Status::BadFrame)?;
                done(&r)?;
                if matches!(self.lease, Some((held, until)) if held != token && until > ctx.now_us)
                {
                    return Err(Status::Busy);
                }
                self.lease = Some((token, ctx.now_us + u64::from(LEASE_MS) * 1000));
            }
            cmd::RELEASE => {
                let token = r.u32().ok_or(Status::BadFrame)?;
                done(&r)?;
                if matches!(self.lease, Some((held, _)) if held == token) {
                    self.lease = None;
                }
            }
            cmd::CATALOG => {
                let offset = usize::from(r.u16().ok_or(Status::BadFrame)?);
                let limit = usize::from(r.u8().ok_or(Status::BadFrame)?);
                done(&r)?;
                w.u16(entries.len() as u16)?;
                let count_at = w.payload_len();
                w.u8(0)?;
                let mut returned = 0u8;
                for e in entries.iter().skip(offset).take(limit) {
                    let size = 4 + 1 + 1 + 4 + 12 + 1 + e.name.len() + 1 + e.unit.len();
                    if !w.fits(size) {
                        break;
                    }
                    let (min, max, step) = e.range();
                    w.u32(e.id)?
                        .u8(e.kind as u8)?
                        .u8(e.access as u8)?
                        .u32(e.default)?
                        .u32(min.to_bits())?
                        .u32(max.to_bits())?
                        .u32(step.to_bits())?
                        .u8(e.name.len() as u8)?
                        .bytes(e.name.as_bytes())?
                        .u8(e.unit.len() as u8)?
                        .bytes(e.unit.as_bytes())?;
                    returned += 1;
                }
                w.patch_u8(count_at, returned);
            }
            cmd::READ => {
                let count = r.u8().ok_or(Status::BadFrame)?;
                if r.remaining() != usize::from(count) * 4 {
                    return Err(Status::BadFrame);
                }
                w.u8(count)?;
                for _ in 0..count {
                    let id = r.u32().ok_or(Status::BadFrame)?;
                    let e = self.find(id).ok_or(Status::UnknownId)?;
                    w.u32(id)?.u32(e.requested_bits())?.u32(e.applied_bits())?;
                }
            }
            cmd::WRITE => {
                let token = r.u32().ok_or(Status::BadFrame)?;
                let id = r.u32().ok_or(Status::BadFrame)?;
                let (tag, bits) = r.slot().ok_or(Status::BadFrame)?;
                done(&r)?;
                self.leased(token, ctx.now_us)?;
                let e = self.find(id).ok_or(Status::UnknownId)?;
                check_access(e.access, ctx.safe)?;
                if tag != e.kind as u8 {
                    return Err(Status::WrongKind);
                }
                if e.kind == Kind::F32 {
                    let value = f32::from_bits(bits);
                    if !(value.is_finite() && value >= e.min && value <= e.max) {
                        return Err(Status::Range);
                    }
                }
                e.store_request(bits);
                w.u32(id)?.u32(e.requested_bits())?;
            }
            cmd::DISCARD => {
                let token = r.u32().ok_or(Status::BadFrame)?;
                done(&r)?;
                self.leased(token, ctx.now_us)?;
                let tunables = entries.iter().filter(|e| e.is_tunable());
                if !ctx.safe && tunables.clone().any(|e| e.access == Access::SafeOnly) {
                    return Err(Status::StateDenied);
                }
                let mut count = 0u16;
                for e in tunables {
                    e.store_request(e.default);
                    count += 1;
                }
                w.u16(count)?;
            }
            cmd::WATCH => {
                let period_ms = r.u16().ok_or(Status::BadFrame)?;
                let count = usize::from(r.u8().ok_or(Status::BadFrame)?);
                if count > MAX_WATCHES || r.remaining() != count * 4 {
                    return Err(Status::BadFrame);
                }
                let mut watches = [0u16; MAX_WATCHES];
                for slot in &mut watches[..count] {
                    let id = r.u32().ok_or(Status::BadFrame)?;
                    let index = entries
                        .iter()
                        .position(|e| e.id == id)
                        .ok_or(Status::UnknownId)?;
                    *slot = index as u16;
                }
                if period_ms == 0 || count == 0 {
                    self.watch_len = 0;
                } else {
                    let frame_bytes = (MAX_FRAME - crate::wire::MAX_PAYLOAD + 9 + 4 * count) as u32;
                    if frame_bytes * 1000 / u32::from(period_ms) > SAMPLE_BUDGET_BYTES_PER_S {
                        return Err(Status::Budget);
                    }
                    self.watches = watches;
                    self.watch_len = count;
                    self.period_us = u64::from(period_ms) * 1000;
                    self.next_sample_us = ctx.now_us;
                }
                w.u8(self.watch_len as u8)?;
            }
            cmd::STATS => {
                done(&r)?;
                w.u32(self.sent)?.u32(self.dropped)?.u32(ctx.bad_frames)?;
            }
            _ => return Err(Status::UnknownCommand),
        }
        Ok(())
    }

    fn find(&self, id: u32) -> Option<&'static crate::Entry> {
        self.table.entries().iter().copied().find(|e| e.id == id)
    }

    fn leased(&self, token: u32, now_us: u64) -> Result<(), Status> {
        match self.lease {
            Some((held, until)) if held == token && until > now_us => Ok(()),
            _ => Err(Status::SessionRequired),
        }
    }

    /// When the next sample frame is due, if anything is watched.
    #[must_use]
    pub const fn next_sample_us(&self) -> Option<u64> {
        if self.watch_len == 0 {
            None
        } else {
            Some(self.next_sample_us)
        }
    }

    /// Write a sample frame into `out` when one is due; returns its length.
    /// Samples missed because the caller came late are counted as dropped.
    pub fn sample(&mut self, now_us: u64, out: &mut [u8; MAX_FRAME]) -> usize {
        if self.watch_len == 0 || now_us < self.next_sample_us {
            return 0;
        }
        let late = (now_us - self.next_sample_us) / self.period_us;
        self.dropped = self.dropped.saturating_add(late as u32);
        self.next_sample_us += (late + 1) * self.period_us;
        let header = Header {
            cmd: cmd::SAMPLE,
            flags: 0,
            seq: self.sample_seq,
        };
        let entries = self.table.entries();
        let written = Writer::new(out, header).and_then(|mut w| {
            w.u64(now_us)?.u8(self.watch_len as u8)?;
            for &index in &self.watches[..self.watch_len] {
                w.u32(entries[usize::from(index)].applied_bits())?;
            }
            Ok(w.finish())
        });
        match written {
            Ok(n) => {
                self.sample_seq = self.sample_seq.wrapping_add(1);
                self.sent = self.sent.saturating_add(1);
                n
            }
            Err(Overflow) => 0,
        }
    }

    /// The caller could not send a frame [`Server::sample`] produced.
    pub fn sample_dropped(&mut self) {
        self.sent = self.sent.saturating_sub(1);
        self.dropped = self.dropped.saturating_add(1);
    }
}

fn check_access(access: Access, safe: bool) -> Result<(), Status> {
    match access {
        Access::ReadOnly => Err(Status::ReadOnly),
        Access::SafeOnly if !safe => Err(Status::StateDenied),
        Access::Live | Access::SafeOnly => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Decoder, MAGIC};
    use crate::{Tunable, WatchU32};

    static GAIN: Tunable = Tunable::new("s.gain", "1/s", 2.0, 0.0, 10.0, 0.5);
    static LIMIT: Tunable = Tunable::new("s.limit", "A", 1.0, 0.0, 5.0, 0.1).safe_only();
    static TICKS: WatchU32 = WatchU32::new("s.ticks", "");
    static TABLE: Table = Table::new(&[GAIN.entry(), LIMIT.entry(), TICKS.entry()]);
    // DISCARD resets every request, so its test owns its values.
    static D_GAIN: Tunable = Tunable::new("d.gain", "", 2.0, 0.0, 10.0, 0.5);
    static D_LIMIT: Tunable = Tunable::new("d.limit", "A", 1.0, 0.0, 5.0, 0.1).safe_only();
    static D_TABLE: Table = Table::new(&[D_GAIN.entry(), D_LIMIT.entry()]);

    const TOKEN: u32 = 0x1234_5678;

    struct Link {
        server: Server,
        out: [u8; MAX_FRAME],
        now_us: u64,
        safe: bool,
        seq: u16,
    }

    struct Reply {
        cmd: u8,
        seq: u16,
        status: u8,
        body: [u8; 256],
        len: usize,
    }

    impl Reply {
        fn body(&self) -> Reader<'_> {
            Reader::new(&self.body[..self.len])
        }
    }

    impl Link {
        fn new() -> Self {
            Self::with(&TABLE)
        }

        fn with(table: &'static Table) -> Self {
            Self {
                server: Server::new(table),
                out: [0; MAX_FRAME],
                now_us: 1_000_000,
                safe: false,
                seq: 0,
            }
        }

        fn call(&mut self, code: u8, payload: &[u8]) -> Reply {
            self.seq += 1;
            let ctx = Context {
                now_us: self.now_us,
                safe: self.safe,
                bad_frames: 3,
            };
            let header = Header {
                cmd: code,
                flags: 0,
                seq: self.seq,
            };
            let n = self.server.handle(ctx, header, payload, &mut self.out);
            decode(&self.out[..n])
        }
    }

    fn decode(bytes: &[u8]) -> Reply {
        assert_eq!(bytes[0], MAGIC);
        let mut reply = None;
        Decoder::new().feed(bytes, |h, p| {
            let mut body = [0; 256];
            body[..p.len() - 1].copy_from_slice(&p[1..]);
            reply = Some(Reply {
                cmd: h.cmd,
                seq: h.seq,
                status: p[0],
                body,
                len: p.len() - 1,
            });
        });
        reply.expect("one valid frame")
    }

    fn write(id: u32, value: f32) -> [u8; 16] {
        let mut p = [0; 16];
        p[..4].copy_from_slice(&TOKEN.to_le_bytes());
        p[4..8].copy_from_slice(&id.to_le_bytes());
        p[8] = Kind::F32 as u8;
        p[12..].copy_from_slice(&value.to_bits().to_le_bytes());
        p
    }

    #[test]
    fn hello_describes_the_table() {
        let mut link = Link::new();
        let reply = link.call(cmd::HELLO, &[]);
        assert_eq!(
            (reply.cmd, reply.seq, reply.status),
            (cmd::HELLO | REPLY, 1, 0)
        );
        let mut r = reply.body();
        assert_eq!(r.u8(), Some(crate::wire::VERSION));
        assert_eq!(r.u32(), Some(TABLE_VERSION));
        assert_eq!(r.u16(), Some(3));
        assert_eq!(r.u32(), Some(TABLE.fingerprint()));
        assert_eq!(r.u16(), Some(256));
        assert_eq!(r.u16(), Some(LEASE_MS));
    }

    #[test]
    fn the_catalog_pages_through_every_entry() {
        let mut link = Link::new();
        let reply = link.call(cmd::CATALOG, &[1, 0, 10]);
        let mut r = reply.body();
        assert_eq!((r.u16(), r.u8()), (Some(3), Some(2)));
        assert_eq!(r.u32(), Some(LIMIT.entry().id()));
        assert_eq!(r.u8(), Some(Kind::F32 as u8));
        assert_eq!(r.u8(), Some(Access::SafeOnly as u8));
        assert_eq!(r.u32(), Some(1.0f32.to_bits()));
        assert_eq!(r.u32().map(f32::from_bits), Some(0.0));
        assert_eq!(r.u32().map(f32::from_bits), Some(5.0));
        assert_eq!(r.u32().map(f32::from_bits), Some(0.1));
        assert_eq!(r.u8(), Some(7));
    }

    #[test]
    fn volatile_server_rejects_save_without_pending_storage_work() {
        let mut link = Link::new();
        link.server = Server::without_storage(&TABLE);
        let token = TOKEN.to_le_bytes();
        assert_eq!(
            link.call(cmd::SAVE, &token).status,
            Status::SessionRequired as u8
        );
        assert_eq!(link.call(cmd::LEASE, &token).status, 0);
        assert_eq!(
            link.call(cmd::SAVE, &token).status,
            Status::SaveUnsupported as u8
        );
        assert!(!link.server.save_pending());
        assert_eq!(link.server.finish_save(Ok(1), &mut link.out), 0);
        assert_eq!(link.call(cmd::HELLO, &[]).status, 0);
        assert_eq!(link.call(cmd::CATALOG, &[0, 0, 10]).status, 0);
    }

    #[test]
    fn save_waits_for_storage_and_answers_with_the_request_seq() {
        let mut link = Link::new();
        let mut token = [0; 4];
        token.copy_from_slice(&TOKEN.to_le_bytes());
        assert_eq!(
            link.call(cmd::SAVE, &token).status,
            Status::SessionRequired as u8
        );
        assert_eq!(link.call(cmd::LEASE, &token).status, 0);
        let ctx = Context {
            now_us: link.now_us,
            safe: false,
            bad_frames: 0,
        };
        let header = Header {
            cmd: cmd::SAVE,
            flags: 0,
            seq: 40,
        };
        assert_eq!(link.server.handle(ctx, header, &token, &mut link.out), 0);
        assert!(link.server.save_pending());
        assert_eq!(link.call(cmd::SAVE, &token).status, Status::Busy as u8);

        let n = link.server.finish_save(Ok(9), &mut link.out);
        let reply = decode(&link.out[..n]);
        assert_eq!(
            (reply.cmd, reply.seq, reply.status),
            (cmd::SAVE | REPLY, 40, 0)
        );
        assert_eq!(reply.body().u32(), Some(9));
        assert!(!link.server.save_pending());
        assert_eq!(link.server.finish_save(Ok(10), &mut link.out), 0);

        assert_eq!(link.server.handle(ctx, header, &token, &mut link.out), 0);
        let n = link.server.finish_save(Err(Status::Storage), &mut link.out);
        assert_eq!(decode(&link.out[..n]).status, Status::Storage as u8);
    }

    #[test]
    fn writes_need_the_lease() {
        let mut link = Link::new();
        let id = GAIN.entry().id();
        assert_eq!(
            link.call(cmd::WRITE, &write(id, 3.0)).status,
            Status::SessionRequired as u8
        );
        assert_eq!(link.call(cmd::LEASE, &TOKEN.to_le_bytes()).status, 0);
        let reply = link.call(cmd::WRITE, &write(id, 3.0));
        assert_eq!(reply.status, 0);
        assert_eq!(GAIN.requested(), 3.0);

        assert_eq!(
            link.call(cmd::LEASE, &7u32.to_le_bytes()).status,
            Status::Busy as u8,
            "another host cannot take a held lease"
        );
        link.now_us += u64::from(LEASE_MS) * 1000;
        assert_eq!(
            link.call(cmd::WRITE, &write(id, 4.0)).status,
            Status::SessionRequired as u8
        );
        assert_eq!(
            link.call(cmd::LEASE, &7u32.to_le_bytes()).status,
            0,
            "a lapsed lease is free"
        );
    }

    #[test]
    fn writes_are_checked_against_the_descriptor() {
        let mut link = Link::new();
        link.call(cmd::LEASE, &TOKEN.to_le_bytes());
        let gain = GAIN.entry().id();
        assert_eq!(
            link.call(cmd::WRITE, &write(gain, 11.0)).status,
            Status::Range as u8
        );
        assert_eq!(
            link.call(cmd::WRITE, &write(gain, f32::NAN)).status,
            Status::Range as u8
        );
        assert_eq!(
            link.call(cmd::WRITE, &write(7, 1.0)).status,
            Status::UnknownId as u8
        );
        assert_eq!(
            link.call(cmd::WRITE, &write(TICKS.entry().id(), 1.0))
                .status,
            Status::ReadOnly as u8
        );
        let mut wrong = write(gain, 1.0);
        wrong[8] = Kind::U32 as u8;
        assert_eq!(
            link.call(cmd::WRITE, &wrong).status,
            Status::WrongKind as u8
        );
        assert_eq!(
            link.call(cmd::WRITE, &write(gain, 1.0)[..12]).status,
            Status::BadFrame as u8
        );
    }

    #[test]
    fn safe_only_values_change_only_while_safe() {
        let mut link = Link::with(&D_TABLE);
        link.call(cmd::LEASE, &TOKEN.to_le_bytes());
        let limit = D_LIMIT.entry().id();
        assert_eq!(
            link.call(cmd::WRITE, &write(limit, 2.0)).status,
            Status::StateDenied as u8
        );
        assert_eq!(
            link.call(cmd::DISCARD, &TOKEN.to_le_bytes()).status,
            Status::StateDenied as u8
        );
        link.safe = true;
        assert_eq!(link.call(cmd::WRITE, &write(limit, 2.0)).status, 0);
        assert_eq!(D_LIMIT.requested(), 2.0);
        let reply = link.call(cmd::DISCARD, &TOKEN.to_le_bytes());
        assert_eq!((reply.status, reply.body().u16()), (0, Some(2)));
        assert_eq!(D_LIMIT.requested(), 1.0);
    }

    #[test]
    fn read_returns_requested_and_applied() {
        let mut link = Link::new();
        TICKS.publish(42);
        let mut p = [0; 5];
        p[0] = 1;
        p[1..].copy_from_slice(&TICKS.entry().id().to_le_bytes());
        let reply = link.call(cmd::READ, &p);
        let mut r = reply.body();
        assert_eq!(r.u8(), Some(1));
        assert_eq!(r.u32(), Some(TICKS.entry().id()));
        assert_eq!(r.u32(), Some(0));
        assert_eq!(r.u32(), Some(42));
    }

    fn watch(period_ms: u16, ids: &[u32]) -> ([u8; 256], usize) {
        let mut p = [0; 256];
        p[..2].copy_from_slice(&period_ms.to_le_bytes());
        p[2] = ids.len() as u8;
        for (i, id) in ids.iter().enumerate() {
            p[3 + 4 * i..7 + 4 * i].copy_from_slice(&id.to_le_bytes());
        }
        (p, 3 + 4 * ids.len())
    }

    #[test]
    fn a_subscription_produces_samples_on_its_period() {
        let mut link = Link::new();
        let ids = [TICKS.entry().id(), GAIN.entry().id()];
        let (p, n) = watch(10, &ids);
        assert_eq!(link.call(cmd::WATCH, &p[..n]).status, 0);
        TICKS.publish(9);

        let start = link.now_us;
        let mut out = [0; MAX_FRAME];
        let n = link.server.sample(start, &mut out);
        let mut got = None;
        Decoder::new().feed(&out[..n], |h, p| {
            let mut r = Reader::new(p);
            let time = u64::from(r.u32().unwrap()) | (u64::from(r.u32().unwrap()) << 32);
            got = Some((h.cmd, h.seq, time, r.u8(), r.u32()));
        });
        assert_eq!(got, Some((cmd::SAMPLE, 0, start, Some(2), Some(9))));
        assert_eq!(
            link.server.sample(start + 5_000, &mut out),
            0,
            "not due yet"
        );
        assert_eq!(link.server.next_sample_us(), Some(start + 10_000));

        assert_ne!(link.server.sample(start + 35_000, &mut out), 0);
        assert_eq!(link.server.next_sample_us(), Some(start + 40_000));
        let stats = link.call(cmd::STATS, &[]);
        let mut r = stats.body();
        assert_eq!((r.u32(), r.u32(), r.u32()), (Some(2), Some(2), Some(3)));
    }

    #[test]
    fn a_subscription_over_budget_is_refused() {
        let mut link = Link::new();
        let (p, n) = watch(1, &[TICKS.entry().id(); MAX_WATCHES + 1]);
        assert_eq!(
            link.call(cmd::WATCH, &p[..n]).status,
            Status::BadFrame as u8
        );
        let (p, n) = watch(1, &[TICKS.entry().id(); 20]);
        assert_eq!(link.call(cmd::WATCH, &p[..n]).status, Status::Budget as u8);
        let (p, n) = watch(0, &[]);
        assert_eq!(link.call(cmd::WATCH, &p[..n]).status, 0);
        assert_eq!(link.server.next_sample_us(), None);
    }

    #[test]
    fn an_unknown_command_is_answered() {
        let mut link = Link::new();
        let reply = link.call(0x7f, &[]);
        assert_eq!(
            (reply.cmd, reply.status),
            (0xff, Status::UnknownCommand as u8)
        );
    }
}
