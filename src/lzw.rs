use std::io::{self, BufRead, Read};

const MAGIC: [u8; 2] = [0x1F, 0x9D];
const BLOCK_MODE: u8 = 0x80;
const MAX_BITS_MASK: u8 = 0x1F;
const RESERVED: u8 = 0x60;
const INITIAL_BITS: u32 = 9;
const SMALLEST_MAX_BITS: u32 = 9;
const LARGEST_MAX_BITS: u32 = 16;
const CLEAR: u32 = 256;
const LITERALS: u32 = 256;

fn data_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

// LINK: https://github.com/vapier/ncompress/blob/main/compress.c
pub(crate) struct Unlzw<R> {
    input: R,
    started: bool,
    block_mode: bool,
    max_bits: u32,
    bits: u32,
    largest: u32,
    group: [u8; LARGEST_MAX_BITS as usize],
    codes_in_group: u32,
    next_in_group: u32,
    prefix: Vec<u16>,
    suffix: Vec<u8>,
    free: u32,
    previous: Option<u32>,
    first_byte: u8,
    pending: Vec<u8>,
    position: usize,
    ended: bool,
}

impl<R: BufRead> Unlzw<R> {
    pub(crate) fn new(input: R) -> Self {
        Unlzw {
            input,
            started: false,
            block_mode: false,
            max_bits: LARGEST_MAX_BITS,
            bits: INITIAL_BITS,
            largest: (1 << INITIAL_BITS) - 1,
            group: [0; LARGEST_MAX_BITS as usize],
            codes_in_group: 0,
            next_in_group: 0,
            prefix: vec![0; 1 << LARGEST_MAX_BITS],
            suffix: vec![0; 1 << LARGEST_MAX_BITS],
            free: 0,
            previous: None,
            first_byte: 0,
            pending: Vec::new(),
            position: 0,
            ended: false,
        }
    }

    fn start(&mut self) -> io::Result<()> {
        let mut header = [0u8; 3];
        self.input
            .read_exact(&mut header)
            .map_err(|_| data_error("truncated compress header"))?;
        if header[..2] != MAGIC {
            return Err(data_error("not a compress (.Z) stream"));
        }
        let max_bits = u32::from(header[2] & MAX_BITS_MASK);
        if header[2] & RESERVED != 0 || !(SMALLEST_MAX_BITS..=LARGEST_MAX_BITS).contains(&max_bits)
        {
            return Err(data_error("compress header has unknown flags or code size"));
        }
        self.block_mode = header[2] & BLOCK_MODE != 0;
        self.max_bits = max_bits;
        self.reset_table();
        self.started = true;
        Ok(())
    }

    fn first_free(&self) -> u32 {
        LITERALS + u32::from(self.block_mode)
    }

    // EXTERNAL(ncompress, BSD zopen, gzip unlzw): the first limit is 511 whatever the largest width, so a 9-bit stream widens to 10 bits.
    fn reset_table(&mut self) {
        self.bits = INITIAL_BITS;
        self.largest = (1 << INITIAL_BITS) - 1;
        self.free = self.first_free();
    }

    fn widen(&mut self) {
        self.bits += 1;
        self.largest = if self.bits == self.max_bits {
            1 << self.max_bits
        } else {
            (1 << self.bits) - 1
        };
    }

    fn next_code(&mut self) -> io::Result<Option<u32>> {
        if self.free > self.largest {
            self.widen();
            self.next_in_group = self.codes_in_group;
        }
        if self.next_in_group == self.codes_in_group {
            let wanted = self.bits as usize;
            let mut filled = 0;
            while filled < wanted {
                let read = self.input.read(&mut self.group[filled..wanted])?;
                if read == 0 {
                    break;
                }
                filled += read;
            }
            self.codes_in_group = (filled as u32 * 8) / self.bits;
            self.next_in_group = 0;
            if self.codes_in_group == 0 {
                return Ok(None);
            }
        }
        let first_bit = self.next_in_group * self.bits;
        self.next_in_group += 1;
        let mut code = 0u32;
        for bit in 0..self.bits {
            let at = first_bit + bit;
            let set = self.group[(at / 8) as usize] >> (at % 8) & 1;
            code |= u32::from(set) << bit;
        }
        Ok(Some(code))
    }

    fn decode_next(&mut self) -> io::Result<bool> {
        let Some(code) = self.next_code()? else {
            return Ok(false);
        };
        let Some(previous) = self.previous else {
            if code >= LITERALS {
                return Err(data_error(
                    "compress stream starts with a code that is not a byte",
                ));
            }
            self.previous = Some(code);
            self.first_byte = code as u8;
            self.pending.push(code as u8);
            return Ok(true);
        };
        if self.block_mode && code == CLEAR {
            self.reset_table();
            self.free -= 1;
            self.next_in_group = self.codes_in_group;
            return Ok(true);
        }
        let start = self.pending.len();
        let mut walk = code;
        if code >= self.free {
            if code > self.free {
                return Err(data_error(
                    "compress stream names a code it has not defined",
                ));
            }
            self.pending.push(self.first_byte);
            walk = previous;
        }
        while walk >= LITERALS {
            self.pending.push(self.suffix[walk as usize]);
            walk = u32::from(self.prefix[walk as usize]);
        }
        self.first_byte = walk as u8;
        self.pending.push(self.first_byte);
        self.pending[start..].reverse();
        if self.free < 1 << self.max_bits {
            self.prefix[self.free as usize] = previous as u16;
            self.suffix[self.free as usize] = self.first_byte;
            self.free += 1;
        }
        self.previous = Some(code);
        Ok(true)
    }
}

impl<R: BufRead> Read for Unlzw<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !self.started {
            self.start()?;
        }
        while self.position == self.pending.len() && !self.ended && !buffer.is_empty() {
            self.pending.clear();
            self.position = 0;
            while self.pending.len() < 64 * 1024 {
                if !self.decode_next()? {
                    self.ended = true;
                    break;
                }
            }
        }
        let count = buffer.len().min(self.pending.len() - self.position);
        buffer[..count].copy_from_slice(&self.pending[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(data: &[u8]) -> Option<Vec<u8>> {
        reference_noting_clears(data, &mut Vec::new())
    }

    fn reference_noting_clears(data: &[u8], clear_widths: &mut Vec<usize>) -> Option<Vec<u8>> {
        if data.len() < 3 || data[..2] != MAGIC || data[2] & RESERVED != 0 {
            return None;
        }
        let max_bits = (data[2] & MAX_BITS_MASK) as usize;
        if !(9..=16).contains(&max_bits) {
            return None;
        }
        let block_mode = data[2] & BLOCK_MODE != 0;
        let body = &data[3..];
        let total_bits = body.len() * 8;
        let first = if block_mode { 257 } else { 256 };
        let (mut bits, mut position, mut group_start) = (9usize, 0usize, 0usize);
        let mut free = first;
        let mut table: Vec<(usize, u8)> = vec![(0, 0); 1 << 16];
        let mut previous: Option<usize> = None;
        let mut last_first = 0u8;
        let mut out = Vec::new();
        let round_up = |position: usize, group_start: usize, bits: usize| {
            let into = position - group_start;
            group_start + into.div_ceil(bits * 8) * bits * 8
        };
        let mut limit = 511usize;
        loop {
            if free > limit {
                position = round_up(position, group_start, bits);
                group_start = position;
                bits += 1;
                limit = if bits == max_bits {
                    1 << max_bits
                } else {
                    (1 << bits) - 1
                };
            }
            if position + bits > total_bits {
                return Some(out);
            }
            let mut code = 0usize;
            for bit in 0..bits {
                let at = position + bit;
                code |= ((body[at / 8] >> (at % 8) & 1) as usize) << bit;
            }
            position += bits;
            let Some(old) = previous else {
                if code >= 256 {
                    return None;
                }
                out.push(code as u8);
                last_first = code as u8;
                previous = Some(code);
                continue;
            };
            if block_mode && code == 256 {
                clear_widths.push(bits);
                position = round_up(position, group_start, bits);
                group_start = position;
                bits = 9;
                limit = 511;
                free = first - 1;
                continue;
            }
            let mut string = Vec::new();
            let mut walk = code;
            if code >= free {
                if code > free {
                    return None;
                }
                string.push(last_first);
                walk = old;
            }
            while walk >= 256 {
                string.push(table[walk].1);
                walk = table[walk].0;
            }
            string.push(walk as u8);
            string.reverse();
            last_first = string[0];
            out.extend_from_slice(&string);
            if free < 1 << max_bits {
                table[free] = (old, last_first);
                free += 1;
            }
            previous = Some(code);
        }
    }

    fn decode(data: &[u8]) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        Unlzw::new(data).read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn every_pseudo_random_stream_decodes_as_the_reference_does() {
        let mut state = 1u32;
        let mut next = || {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 16) as u8
        };
        let mut checked = 0;
        for flags in [0x89, 0x8C, 0x90, 0x09, 0x10] {
            for length in 0..300 {
                for _ in 0..40 {
                    let mut data = vec![0x1F, 0x9D, flags];
                    let skew = next();
                    data.extend((0..length).map(|_| next() & (skew | 0x0F)));
                    let expected = reference(&data);
                    let decoded = decode(&data).ok();
                    assert_eq!(decoded, expected, "flags {flags:02x} {data:02x?}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 50_000, "{checked}");
    }

    fn compress_without_clear(data: &[u8], max_bits: u32) -> Vec<u8> {
        let mut out = vec![0x1F, 0x9D, max_bits as u8];
        let mut group: Vec<u32> = Vec::new();
        let mut bits = INITIAL_BITS;
        let mut free = LITERALS;
        let mut table = std::collections::HashMap::new();
        let flush = |group: &mut Vec<u32>, bits: u32, out: &mut Vec<u8>| {
            let mut packed = vec![0u8; bits as usize];
            for (index, code) in group.iter().enumerate() {
                for bit in 0..bits {
                    let at = index as u32 * bits + bit;
                    packed[(at / 8) as usize] |= ((code >> bit & 1) as u8) << (at % 8);
                }
            }
            let used = (group.len() as u32 * bits).div_ceil(8) as usize;
            out.extend_from_slice(&packed[..used]);
            group.clear();
        };
        let emit =
            |code: u32, free: u32, bits: &mut u32, group: &mut Vec<u32>, out: &mut Vec<u8>| {
                group.push(code);
                if group.len() == 8 {
                    flush(group, *bits, out);
                }
                let largest = if *bits == max_bits {
                    1 << max_bits
                } else {
                    (1 << *bits) - 1
                };
                if free > largest {
                    if !group.is_empty() {
                        let mut padded = std::mem::take(group);
                        padded.resize(8, 0);
                        flush(&mut padded, *bits, out);
                    }
                    *bits += 1;
                }
            };
        let mut current: Option<u32> = None;
        for &byte in data {
            let Some(prefix) = current else {
                current = Some(u32::from(byte));
                continue;
            };
            if let Some(&code) = table.get(&(prefix, byte)) {
                current = Some(code);
                continue;
            }
            emit(prefix, free, &mut bits, &mut group, &mut out);
            if free < 1 << max_bits {
                table.insert((prefix, byte), free);
                free += 1;
            }
            current = Some(u32::from(byte));
        }
        if let Some(prefix) = current {
            emit(prefix, free, &mut bits, &mut group, &mut out);
        }
        if !group.is_empty() {
            flush(&mut group, bits, &mut out);
        }
        out
    }

    #[test]
    fn a_stream_without_clear_codes_changes_width_mid_group_as_7zip_reads_it() {
        if std::process::Command::new("7zz").arg("i").output().is_err() {
            return;
        }
        let work = tempfile::tempdir().unwrap();
        let text: Vec<u8> = (0..120_000u32)
            .map(|index| (index.wrapping_mul(2_654_435_761) >> 29) as u8 + b'a')
            .collect();
        for max_bits in [12, 16] {
            let compressed = compress_without_clear(&text, max_bits);
            let path = work.path().join(format!("text{max_bits}.Z"));
            std::fs::write(&path, &compressed).unwrap();
            let seven = std::process::Command::new("7zz")
                .args(["x", "-so", "-bd"])
                .arg(&path)
                .stderr(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(
                seven.status.success(),
                "7-Zip refused the {max_bits}-bit stream"
            );
            assert_eq!(
                seven.stdout, text,
                "7-Zip disagrees with the test encoder at {max_bits} bits"
            );
            assert_eq!(decode(&compressed).unwrap(), text, "{max_bits} bits");
        }
    }

    fn run_tool(program: &str, args: &[&str], input: &[u8]) -> std::process::Output {
        use std::io::Write;
        let mut child = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let input = input.to_vec();
        let feeder = std::thread::spawn(move || stdin.write_all(&input));
        let output = child.wait_with_output().unwrap();
        let _ = feeder.join().unwrap();
        output
    }

    fn mixed_entropy_text(length: usize, seed: u32) -> Vec<u8> {
        let mut state = seed;
        let mut next = move || {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            state >> 8
        };
        let mut text = Vec::with_capacity(length);
        while text.len() < length {
            let run = (next() % 20_000) as usize + 1;
            match next() % 4 {
                0 => text.extend(std::iter::repeat_n(next() as u8, run)),
                1 => {
                    let letters = next() % 12 + 2;
                    text.extend((0..run).map(|_| b'a' + (next() % letters) as u8));
                }
                2 => text.extend((0..run).map(|_| next() as u8)),
                _ if !text.is_empty() => {
                    let from = next() as usize % text.len();
                    let end = text.len().min(from + run);
                    text.extend_from_within(from..end);
                }
                _ => {}
            }
        }
        text.truncate(length);
        text
    }

    #[test]
    fn system_compress_output_decodes_alike_in_zipper_uncompress_and_gzip() {
        if std::process::Command::new("compress")
            .arg("-V")
            .output()
            .is_err()
        {
            return;
        }
        let mut clear_widths = std::collections::BTreeSet::new();
        let mut seed = 7u32;
        for bits in 9..=16u32 {
            let long = 600_000 >> (16 - bits).min(3);
            let lengths = [0, 1, 2, 3, 17, 255, 1_000, long / 3, long, long + 4_099];
            for length in lengths {
                seed = seed.wrapping_add(0x9E37_79B9);
                let text = mixed_entropy_text(length, seed);
                let compressed = run_tool("compress", &["-c", "-f", &format!("-b{bits}")], &text);
                if text.is_empty() {
                    continue;
                }
                assert_eq!(compressed.stdout[2] & MAX_BITS_MASK, bits as u8);
                let mut widths = Vec::new();
                let expected = reference_noting_clears(&compressed.stdout, &mut widths);
                clear_widths.extend(widths);
                assert_eq!(
                    expected.as_deref(),
                    Some(&text[..]),
                    "reference, {bits} bits"
                );
                // EXTERNAL(macOS uncompress and gzip): both refuse a stream whose largest width is under 12 bits.
                let tools = if bits >= 12 {
                    [("uncompress", "-c"), ("gzip", "-dc")].as_slice()
                } else {
                    &[]
                };
                for &(tool, arg) in tools {
                    let other = run_tool(tool, &[arg], &compressed.stdout);
                    assert_eq!(other.stdout, text, "{tool}, {bits} bits, {length} bytes");
                }
                assert_eq!(
                    decode(&compressed.stdout).unwrap(),
                    text,
                    "{bits} bits, {length} bytes"
                );
            }
        }
        assert_eq!(
            clear_widths.into_iter().collect::<Vec<_>>(),
            (10..=16).collect::<Vec<_>>(),
            "the streams clear a full table at every width, and a 9-bit table fills at 10 bits"
        );
    }

    #[test]
    fn real_compress_output_decodes_at_every_code_size() {
        let Ok(tool) = std::process::Command::new("compress").arg("-V").output() else {
            return;
        };
        let _ = tool;
        let text: Vec<u8> = (0..400_000u32)
            .map(|index| {
                b"the quick brown fox jumps over "[(index % 31) as usize]
                    ^ ((index / 997) as u8 % 3)
            })
            .collect();
        for bits in 9..=16 {
            let compressed = std::process::Command::new("compress")
                .args(["-c", "-f", &format!("-b{bits}")])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    let mut stdin = child.stdin.take().unwrap();
                    let input = text.clone();
                    let feeder = std::thread::spawn(move || stdin.write_all(&input));
                    let output = child.wait_with_output();
                    feeder.join().unwrap()?;
                    output
                })
                .unwrap()
                .stdout;
            assert_eq!(compressed[2] & 0x1F, bits as u8);
            assert_eq!(decode(&compressed).unwrap(), text, "{bits} bits");
            assert_eq!(
                reference(&compressed).unwrap(),
                text,
                "reference at {bits} bits"
            );
        }
    }
}
