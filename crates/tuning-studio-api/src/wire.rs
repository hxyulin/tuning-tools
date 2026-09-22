//! Frames for a byte-stream link (USB CDC, RTT, UART).
//!
//! ```text
//! magic u8 | version u8 | cmd u8 | flags u8 | seq u16 | len u16 | payload | crc16
//! ```
//!
//! Little-endian throughout. The CRC is CRC-16/MCRF4XX over the header and
//! payload. A reply carries `cmd | REPLY`, the request's `seq`, and a payload
//! that starts with a [`Status`] byte. Asynchronous frames (samples) number
//! their own `seq`, so a gap shows a dropped frame.

pub const MAGIC: u8 = 0xa5;
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 8;
pub const MAX_PAYLOAD: usize = 256;
pub const MAX_FRAME: usize = HEADER_LEN + MAX_PAYLOAD + 2;
/// Set on a reply's `cmd`.
pub const REPLY: u8 = 0x80;

/// Command codes. Replies use `code | REPLY`.
pub mod cmd {
    /// → nothing. ← protocol version u8, table version u32, entries u16,
    /// fingerprint u32, max payload u16, lease u16 ms.
    pub const HELLO: u8 = 0x01;
    /// → token u32. ← nothing. Takes the lease, or renews one held by `token`.
    pub const LEASE: u8 = 0x02;
    /// → token u32. ← nothing.
    pub const RELEASE: u8 = 0x03;
    /// → offset u16, limit u8. ← total u16, returned u8, then per entry: id u32,
    /// kind u8, access u8, default u32, min f32, max f32, max step f32,
    /// name (len u8, bytes), unit (len u8, bytes).
    pub const CATALOG: u8 = 0x10;
    /// → count u8, ids u32. ← count u8, then per id: id u32, requested u32,
    /// applied u32.
    pub const READ: u8 = 0x11;
    /// → token u32, id u32, value slot. ← id u32, requested u32.
    pub const WRITE: u8 = 0x12;
    /// → token u32. ← count u16. Every tunable's request back to its default.
    pub const DISCARD: u8 = 0x13;
    /// → token u32. ← generation u32. Every tunable's request to storage; the
    /// reply comes when the write is verified.
    pub const SAVE: u8 = 0x14;
    /// → period u16 ms, count u8, ids u32. ← count u8. A period of 0 or no ids
    /// stops sampling.
    pub const WATCH: u8 = 0x20;
    /// → nothing. ← samples sent u32, samples dropped u32, bad frames u32.
    pub const STATS: u8 = 0x22;
    /// Unsolicited: time u64 us, count u8, then one u32 per watched id in
    /// subscription order.
    pub const SAMPLE: u8 = 0x40;
}

/// The first byte of every reply payload.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Ok = 0,
    /// The payload is not the command's shape.
    BadFrame = 1,
    UnknownCommand = 2,
    UnknownId = 3,
    ReadOnly = 4,
    /// Not a number, or outside the declared range.
    Range = 5,
    /// The value may change only while the robot is safe.
    StateDenied = 6,
    /// The value slot's type tag is not the entry's kind.
    WrongKind = 7,
    /// Writing needs the lease, and this token does not hold it.
    SessionRequired = 8,
    /// Another token holds the lease.
    Busy = 9,
    /// The subscription would exceed the sample byte budget.
    Budget = 10,
    /// Storage is missing, or the write did not verify.
    Storage = 11,
    /// This firmware intentionally has no persistence implementation.
    SaveUnsupported = 12,
}

/// CRC-16/MCRF4XX: reflected 0x1021, init 0xffff, no final xor.
#[must_use]
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &byte in bytes {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            };
        }
    }
    crc
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub cmd: u8,
    pub flags: u8,
    pub seq: u16,
}

/// Reassembles frames from a byte stream, resynchronising on the magic byte
/// after noise, a bad length or a bad CRC.
#[derive(Debug)]
pub struct Decoder {
    buf: [u8; MAX_FRAME],
    len: usize,
    bad: u32,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [0; MAX_FRAME],
            len: 0,
            bad: 0,
        }
    }

    /// Frames dropped for a bad length, version or CRC.
    #[must_use]
    pub const fn bad_frames(&self) -> u32 {
        self.bad
    }

    /// Feed received bytes; `on_frame` runs for each complete, valid frame.
    pub fn feed(&mut self, bytes: &[u8], mut on_frame: impl FnMut(Header, &[u8])) {
        for &byte in bytes {
            if self.len == 0 && byte != MAGIC {
                continue;
            }
            self.buf[self.len] = byte;
            self.len += 1;
            self.parse(&mut on_frame);
        }
    }

    fn parse(&mut self, on_frame: &mut impl FnMut(Header, &[u8])) {
        loop {
            if self.len < HEADER_LEN {
                return;
            }
            let payload_len = usize::from(u16::from_le_bytes([self.buf[6], self.buf[7]]));
            if self.buf[1] != VERSION || payload_len > MAX_PAYLOAD {
                self.bad += 1;
                self.skip();
                continue;
            }
            let total = HEADER_LEN + payload_len + 2;
            if self.len < total {
                return;
            }
            let body = HEADER_LEN + payload_len;
            let crc = u16::from_le_bytes([self.buf[body], self.buf[body + 1]]);
            if crc != crc16(&self.buf[..body]) {
                self.bad += 1;
                self.skip();
                continue;
            }
            let header = Header {
                cmd: self.buf[2],
                flags: self.buf[3],
                seq: u16::from_le_bytes([self.buf[4], self.buf[5]]),
            };
            on_frame(header, &self.buf[HEADER_LEN..body]);
            self.buf.copy_within(total..self.len, 0);
            self.len -= total;
            self.skip_to_magic();
        }
    }

    /// Drop the leading magic and look for the next one.
    fn skip(&mut self) {
        self.buf.copy_within(1..self.len, 0);
        self.len -= 1;
        self.skip_to_magic();
    }

    fn skip_to_magic(&mut self) {
        let start = self.buf[..self.len]
            .iter()
            .position(|&b| b == MAGIC)
            .unwrap_or(self.len);
        self.buf.copy_within(start..self.len, 0);
        self.len -= start;
    }
}

/// The payload did not fit in [`MAX_PAYLOAD`] or the output buffer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Overflow;

/// Builds one frame in place.
#[derive(Debug)]
pub struct Writer<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl<'a> Writer<'a> {
    /// # Errors
    ///
    /// When `out` cannot hold even an empty frame.
    pub fn new(out: &'a mut [u8], header: Header) -> Result<Self, Overflow> {
        if out.len() < HEADER_LEN + 2 {
            return Err(Overflow);
        }
        out[0] = MAGIC;
        out[1] = VERSION;
        out[2] = header.cmd;
        out[3] = header.flags;
        out[4..6].copy_from_slice(&header.seq.to_le_bytes());
        Ok(Self {
            out,
            len: HEADER_LEN,
        })
    }

    /// Payload bytes written so far.
    #[must_use]
    pub const fn payload_len(&self) -> usize {
        self.len - HEADER_LEN
    }

    /// Whether `n` more payload bytes fit.
    #[must_use]
    pub fn fits(&self, n: usize) -> bool {
        self.payload_len() + n <= MAX_PAYLOAD && self.len + n + 2 <= self.out.len()
    }

    /// # Errors
    ///
    /// When the bytes do not fit.
    pub fn bytes(&mut self, bytes: &[u8]) -> Result<&mut Self, Overflow> {
        if !self.fits(bytes.len()) {
            return Err(Overflow);
        }
        self.out[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        Ok(self)
    }

    /// # Errors
    ///
    /// When the byte does not fit.
    pub fn u8(&mut self, v: u8) -> Result<&mut Self, Overflow> {
        self.bytes(&[v])
    }

    /// # Errors
    ///
    /// When the value does not fit.
    pub fn u16(&mut self, v: u16) -> Result<&mut Self, Overflow> {
        self.bytes(&v.to_le_bytes())
    }

    /// # Errors
    ///
    /// When the value does not fit.
    pub fn u32(&mut self, v: u32) -> Result<&mut Self, Overflow> {
        self.bytes(&v.to_le_bytes())
    }

    /// # Errors
    ///
    /// When the value does not fit.
    pub fn u64(&mut self, v: u64) -> Result<&mut Self, Overflow> {
        self.bytes(&v.to_le_bytes())
    }

    /// Overwrite a `u8` already written, at payload offset `at`.
    pub fn patch_u8(&mut self, at: usize, v: u8) {
        self.out[HEADER_LEN + at] = v;
    }

    /// Length and CRC; returns the frame's size in bytes.
    #[must_use]
    pub fn finish(self) -> usize {
        let payload = self.payload_len() as u16;
        self.out[6..8].copy_from_slice(&payload.to_le_bytes());
        let crc = crc16(&self.out[..self.len]);
        self.out[self.len..self.len + 2].copy_from_slice(&crc.to_le_bytes());
        self.len + 2
    }
}

/// Reads a payload front to back; every read fails once it runs out.
#[derive(Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.bytes.len()
    }

    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.bytes.split_first_chunk::<N>()?;
        self.bytes = rest;
        Some(*head)
    }

    pub fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|[b]| b)
    }

    pub fn u16(&mut self) -> Option<u16> {
        self.take().map(u16::from_le_bytes)
    }

    pub fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }

    /// An 8-byte value slot: type tag u8, three reserved bytes, bits u32.
    pub fn slot(&mut self) -> Option<(u8, u32)> {
        let raw: [u8; 8] = self.take()?;
        Some((raw[0], u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_mcrf4xx_check_value() {
        assert_eq!(crc16(b"123456789"), 0x6f91);
    }

    /// The same bytes are asserted by the host's codec tests.
    #[test]
    fn a_hello_request_encodes_to_the_shared_vector() {
        let mut out = [0; MAX_FRAME];
        let n = Writer::new(
            &mut out,
            Header {
                cmd: cmd::HELLO,
                flags: 0,
                seq: 0x0102,
            },
        )
        .unwrap()
        .finish();
        let crc = crc16(&[0xa5, 0x01, 0x01, 0x00, 0x02, 0x01, 0x00, 0x00]).to_le_bytes();
        assert_eq!(
            &out[..n],
            &[0xa5, 0x01, 0x01, 0x00, 0x02, 0x01, 0x00, 0x00, crc[0], crc[1]]
        );
    }

    fn frame(cmd: u8, seq: u16, payload: &[u8]) -> ([u8; MAX_FRAME], usize) {
        let mut out = [0; MAX_FRAME];
        let mut w = Writer::new(&mut out, Header { cmd, flags: 0, seq }).unwrap();
        w.bytes(payload).unwrap();
        let n = w.finish();
        (out, n)
    }

    fn decode_all(d: &mut Decoder, bytes: &[u8]) -> ([(u8, u16, usize); 4], usize) {
        let mut got = [(0, 0, 0); 4];
        let mut n = 0;
        d.feed(bytes, |h, p| {
            got[n] = (h.cmd, h.seq, p.len());
            n += 1;
        });
        (got, n)
    }

    #[test]
    fn frames_split_across_feeds_are_reassembled() {
        let (bytes, n) = frame(cmd::READ, 7, &[1, 2, 3]);
        let mut d = Decoder::new();
        assert_eq!(decode_all(&mut d, &bytes[..5]).1, 0);
        let (got, count) = decode_all(&mut d, &bytes[5..n]);
        assert_eq!((count, got[0]), (1, (cmd::READ, 7, 3)));
    }

    #[test]
    fn noise_and_a_corrupt_frame_are_skipped() {
        let (good, n) = frame(cmd::HELLO, 1, &[]);
        let (mut bad, m) = frame(cmd::READ, 2, &[9; 10]);
        bad[9] ^= 0xff;
        let mut stream = [0u8; 64];
        stream[..3].copy_from_slice(&[0x00, 0xa5, 0x13]);
        stream[3..3 + m].copy_from_slice(&bad[..m]);
        stream[3 + m..3 + m + n].copy_from_slice(&good[..n]);
        let mut d = Decoder::new();
        let (got, count) = decode_all(&mut d, &stream[..3 + m + n]);
        assert_eq!((count, got[0]), (1, (cmd::HELLO, 1, 0)));
        assert!(d.bad_frames() >= 1);
    }

    #[test]
    fn a_payload_past_the_limit_does_not_fit() {
        let mut out = [0; MAX_FRAME + 16];
        let mut w = Writer::new(
            &mut out,
            Header {
                cmd: 1,
                flags: 0,
                seq: 0,
            },
        )
        .unwrap();
        assert!(w.bytes(&[0; MAX_PAYLOAD]).is_ok());
        assert_eq!(w.u8(0).err(), Some(Overflow));
    }

    #[test]
    fn a_reader_stops_at_the_end() {
        let mut r = Reader::new(&[1, 0, 2]);
        assert_eq!(r.u16(), Some(1));
        assert_eq!(r.u16(), None);
        assert_eq!(r.u8(), Some(2));
    }
}
