//! A reader for the parts of an FST file the waves tests check: the header block and the
//! time table of every value-change block, in the format of `GTKWave`'s `fstapi.c` as
//! Verilator 5.052's fstcpp writer emits it (`fstcpp_writer.cpp`: `writeHeader_` and
//! `flushValueChangeDataConstPart_`).
//!
//! Every block is `type: u8`, `length: u64` (big-endian, counting itself but not the type
//! byte), then the payload. A value-change block ends with its time table: the LEB128
//! deltas of its timestamps (zlib-compressed when that is smaller), followed by three
//! `u64`s: uncompressed length, stored length, timestamp count.

use std::path::Path;

/// `FST_BL_HDR`.
const HEADER: u8 = 0;
/// `FST_BL_VCDATA`, `FST_BL_VCDATA_DYN_ALIAS`, `FST_BL_VCDATA_DYN_ALIAS2`.
const VALUE_CHANGES: [u8; 3] = [1, 5, 8];

/// What the tests read from an FST file.
#[derive(Debug)]
pub(crate) struct Fst {
    /// The header's first and last sample times, in ticks.
    pub(crate) start: u64,
    pub(crate) end: u64,
    /// The header's variable count.
    pub(crate) vars: u64,
    /// The header's timescale: one tick is 10^timescale s.
    pub(crate) timescale: i8,
    /// The header's count of value-change blocks.
    pub(crate) blocks: u64,
    /// Every sample time, from the value-change blocks' time tables, in file order.
    pub(crate) times: Vec<u64>,
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let word: [u8; 8] = bytes[at..at + 8].try_into().expect("8 bytes");
    u64::from_be_bytes(word)
}

/// Reads `path`, checking that its blocks chain exactly to the end of the file.
pub(crate) fn read(path: &Path) -> Fst {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut header = None;
    let mut times = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let kind = bytes[at];
        let len = usize::try_from(u64_at(&bytes, at + 1)).expect("a block length");
        assert!(
            len >= 8 && at + 1 + len <= bytes.len(),
            "block {kind} at {at}: length {len} runs past the end ({} bytes)",
            bytes.len()
        );
        let payload = &bytes[at + 9..at + 1 + len];
        if kind == HEADER {
            header = Some(Fst {
                start: u64_at(payload, 0),
                end: u64_at(payload, 8),
                vars: u64_at(payload, 40),
                blocks: u64_at(payload, 56),
                timescale: i8::from_be_bytes([payload[64]]),
                times: Vec::new(),
            });
        } else if VALUE_CHANGES.contains(&kind) {
            times.extend(time_table(payload));
        }
        at += 1 + len;
    }
    let mut fst = header.unwrap_or_else(|| panic!("{}: no header block", path.display()));
    fst.times = times;
    fst
}

/// The timestamps of one value-change block.
fn time_table(payload: &[u8]) -> Vec<u64> {
    let n = payload.len();
    let uncompressed = usize::try_from(u64_at(payload, n - 24)).expect("a length");
    let stored = usize::try_from(u64_at(payload, n - 16)).expect("a length");
    let count = usize::try_from(u64_at(payload, n - 8)).expect("a count");
    let data = &payload[n - 24 - stored..n - 24];
    let raw = if stored == uncompressed {
        data.to_vec()
    } else {
        miniz_oxide::inflate::decompress_to_vec_zlib(data).expect("a zlib time table")
    };
    let mut times = Vec::with_capacity(count);
    let mut time = 0u64;
    let mut iter = raw.iter();
    for _ in 0..count {
        let mut delta = 0u64;
        let mut shift = 0;
        loop {
            let byte = *iter.next().expect("a LEB128 byte");
            delta |= u64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                break;
            }
        }
        time += delta;
        times.push(time);
    }
    times
}
