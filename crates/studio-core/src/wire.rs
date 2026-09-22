//! Host adapters for the shared tuning-studio-api framed protocol.
//!
//! Framing, CRC, constants and decoding come directly from the firmware API.
//!
//! ```text
//! magic u8 | version u8 | cmd u8 | flags u8 | seq u16 | len u16 | payload | crc16
//! ```

pub use tuning_studio_api::wire::{cmd, crc16, HEADER_LEN, MAGIC, MAX_PAYLOAD, REPLY, VERSION};

/// A reply's status byte, as a message for the person who asked.
pub fn status_message(status: u8) -> &'static str {
    match status {
        0 => "ok",
        1 => "the firmware could not parse the request",
        2 => "the firmware does not know this command",
        3 => "the firmware has no value with this id",
        4 => "this value is read-only",
        5 => "the value is outside the range the firmware allows",
        6 => "the firmware only allows this change in a safe state",
        7 => "the value's type does not match the firmware's",
        8 => "this session no longer holds the tuning lease",
        9 => "another tool holds the tuning lease",
        10 => "the watch list would exceed the firmware's sample budget",
        11 => "the firmware could not write its flash",
        _ => "the firmware refused the request",
    }
}

pub const STATUS_BUDGET: u8 = tuning_studio_api::wire::Status::Budget as u8;
pub const STATUS_BUSY: u8 = tuning_studio_api::wire::Status::Busy as u8;

pub fn encode(cmd: u8, seq: u16, payload: &[u8]) -> Vec<u8> {
    use tuning_studio_api::wire::{Header, Writer, MAX_FRAME};
    let mut out = [0; MAX_FRAME];
    let mut writer = Writer::new(&mut out, Header { cmd, flags: 0, seq }).unwrap();
    writer.bytes(payload).expect("payload too long");
    let len = writer.finish();
    out[..len].to_vec()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub cmd: u8,
    pub seq: u16,
    pub payload: Vec<u8>,
}

/// Host adapter around the same bounded codec used by firmware.
#[derive(Debug, Default)]
pub struct Decoder {
    inner: tuning_studio_api::wire::Decoder,
    pub bad_frames: u64,
}
impl Decoder {
    pub fn feed(&mut self, bytes: &[u8], mut on_frame: impl FnMut(Frame)) {
        let before = self.inner.bad_frames();
        self.inner.feed(bytes, |header, payload| {
            on_frame(Frame {
                cmd: header.cmd,
                seq: header.seq,
                payload: payload.to_vec(),
            })
        });
        self.bad_frames += u64::from(self.inner.bad_frames().wrapping_sub(before));
    }
}

/// Reads a payload front to back.
#[derive(Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub fn remaining(&self) -> usize {
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

    pub fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_le_bytes)
    }

    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.bytes.split_at_checked(n)?;
        self.bytes = rest;
        Some(head)
    }

    /// A length-prefixed UTF-8 string.
    pub fn text(&mut self) -> Option<String> {
        let n = usize::from(self.u8()?);
        self.bytes(n)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }
}

/// An 8-byte value slot: type tag, three reserved bytes, bits.
pub fn slot(tag: u8, bits: u32) -> [u8; 8] {
    let mut out = [0; 8];
    out[0] = tag;
    out[4..].copy_from_slice(&bits.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_mcrf4xx_check_value() {
        assert_eq!(crc16(b"123456789"), 0x6f91);
    }

    /// The same bytes are asserted by the firmware's codec tests.
    #[test]
    fn a_hello_request_encodes_to_the_shared_vector() {
        let header = [0xa5, 0x01, 0x01, 0x00, 0x02, 0x01, 0x00, 0x00];
        let crc = crc16(&header).to_le_bytes();
        let mut expected = header.to_vec();
        expected.extend_from_slice(&crc);
        assert_eq!(encode(cmd::HELLO, 0x0102, &[]), expected);
    }

    #[test]
    fn noise_split_frames_and_corruption_are_handled() {
        let good = encode(cmd::READ | REPLY, 7, &[0, 1, 2]);
        let mut bad = encode(cmd::STATS, 8, &[9; 12]);
        bad[10] ^= 0x55;
        let mut stream = vec![0x00, MAGIC, 0x13];
        stream.extend_from_slice(&bad);
        stream.extend_from_slice(&good);
        let mut d = Decoder::default();
        let mut got = Vec::new();
        let (head, tail) = stream.split_at(stream.len() - 4);
        d.feed(head, |f| got.push(f));
        assert!(got.is_empty());
        d.feed(tail, |f| got.push(f));
        assert_eq!(
            got,
            vec![Frame {
                cmd: cmd::READ | REPLY,
                seq: 7,
                payload: vec![0, 1, 2]
            }]
        );
        assert!(d.bad_frames >= 1);
    }

    #[test]
    fn a_reader_reads_strings_and_stops_at_the_end() {
        let mut r = Reader::new(&[2, b'h', b'i', 5]);
        assert_eq!(r.text().as_deref(), Some("hi"));
        assert_eq!(r.u16(), None);
        assert_eq!(r.u8(), Some(5));
    }
}
