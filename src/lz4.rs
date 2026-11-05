use std::io::{self, BufRead, Read};

const FRAME_MAGIC: u32 = 0x184D_2204;
const SKIPPABLE_MAGIC: u32 = 0x184D_2A50;
const SKIPPABLE_MASK: u32 = 0xFFFF_FFF0;
const WINDOW: usize = 64 * 1024;
const HISTORY_HELD: usize = 16 * WINDOW;
const LEGACY_MAGIC: u32 = 0x184C_2102;
const MIN_MATCH: usize = 4;
const UNCOMPRESSED_BLOCK: u32 = 1 << 31;
const LARGEST_BLOCK: usize = 4 * 1024 * 1024;
// SPEC(LZ4 Frame Format 1.6.4): a header of at most 19 bytes, then a block's size, data and checksum.
pub(crate) const FIRST_BLOCK_INPUT_MAX: u64 = 19 + 4 + LARGEST_BLOCK as u64 + 4;
const PRIMES: [u32; 5] = [
    2_654_435_761,
    2_246_822_519,
    3_266_489_917,
    668_265_263,
    374_761_393,
];

fn data_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

// LINK: https://github.com/Cyan4973/xxHash/blob/dev/doc/xxhash_spec.md
pub(crate) struct Xxh32 {
    lanes: [u32; 4],
    held: [u8; 16],
    held_len: usize,
    total: u64,
}

impl Xxh32 {
    pub(crate) fn new() -> Self {
        Xxh32 {
            lanes: [
                PRIMES[0].wrapping_add(PRIMES[1]),
                PRIMES[1],
                0,
                0u32.wrapping_sub(PRIMES[0]),
            ],
            held: [0; 16],
            held_len: 0,
            total: 0,
        }
    }

    fn round(lane: u32, word: u32) -> u32 {
        lane.wrapping_add(word.wrapping_mul(PRIMES[1]))
            .rotate_left(13)
            .wrapping_mul(PRIMES[0])
    }

    fn stripe(&mut self, stripe: &[u8]) {
        for (lane, word) in self.lanes.iter_mut().zip(stripe.chunks_exact(4)) {
            *lane = Self::round(*lane, u32::from_le_bytes(word.try_into().unwrap()));
        }
    }

    pub(crate) fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        if self.held_len > 0 {
            let take = (16 - self.held_len).min(data.len());
            self.held[self.held_len..self.held_len + take].copy_from_slice(&data[..take]);
            self.held_len += take;
            data = &data[take..];
            if self.held_len < 16 {
                return;
            }
            let held = self.held;
            self.stripe(&held);
            self.held_len = 0;
        }
        let whole = data.len() / 16 * 16;
        for stripe in data[..whole].chunks_exact(16) {
            self.stripe(stripe);
        }
        let rest = &data[whole..];
        self.held[..rest.len()].copy_from_slice(rest);
        self.held_len = rest.len();
    }

    pub(crate) fn digest(&self) -> u32 {
        let mut value = if self.total >= 16 {
            let [a, b, c, d] = self.lanes;
            a.rotate_left(1)
                .wrapping_add(b.rotate_left(7))
                .wrapping_add(c.rotate_left(12))
                .wrapping_add(d.rotate_left(18))
        } else {
            PRIMES[4]
        };
        value = value.wrapping_add(self.total as u32);
        let rest = &self.held[..self.held_len];
        let words = rest.len() / 4 * 4;
        for word in rest[..words].chunks_exact(4) {
            value = value
                .wrapping_add(u32::from_le_bytes(word.try_into().unwrap()).wrapping_mul(PRIMES[2]))
                .rotate_left(17)
                .wrapping_mul(PRIMES[3]);
        }
        for &byte in &rest[words..] {
            value = value
                .wrapping_add(u32::from(byte).wrapping_mul(PRIMES[4]))
                .rotate_left(11)
                .wrapping_mul(PRIMES[0]);
        }
        value ^= value >> 15;
        value = value.wrapping_mul(PRIMES[1]);
        value ^= value >> 13;
        value = value.wrapping_mul(PRIMES[2]);
        value ^ (value >> 16)
    }
}

pub(crate) fn xxh32(data: &[u8]) -> u32 {
    let mut hash = Xxh32::new();
    hash.update(data);
    hash.digest()
}

/// Decodes one LZ4 block onto `output`, whose existing bytes are the window
/// that matches may reach. Returns `None` for a malformed block.
pub(crate) fn decode_block(block: &[u8], output: &mut Vec<u8>, limit: usize) -> Option<()> {
    let start = output.len();
    let mut index = 0;
    loop {
        let token = *block.get(index)?;
        index += 1;
        let literals = length(block, &mut index, usize::from(token >> 4))?;
        let end = index.checked_add(literals)?;
        output.extend_from_slice(block.get(index..end)?);
        index = end;
        if index == block.len() {
            return (output.len() - start <= limit).then_some(());
        }
        let offset = usize::from(u16::from_le_bytes([
            *block.get(index)?,
            *block.get(index + 1)?,
        ]));
        index += 2;
        let matched =
            length(block, &mut index, usize::from(token & 0x0F))?.checked_add(MIN_MATCH)?;
        if offset == 0 || offset > output.len() || output.len() - start + matched > limit {
            return None;
        }
        let from = output.len() - offset;
        for position in from..from + matched {
            let byte = output[position];
            output.push(byte);
        }
    }
}

fn length(block: &[u8], index: &mut usize, nibble: usize) -> Option<usize> {
    let mut total = nibble;
    if nibble == 15 {
        loop {
            let more = *block.get(*index)?;
            *index += 1;
            total = total.checked_add(usize::from(more))?;
            if more != 255 {
                break;
            }
        }
    }
    Some(total)
}

struct Frame {
    independent: bool,
    block_checksums: bool,
    content_checksum: bool,
    content_size: Option<u64>,
    block_limit: usize,
    hash: Xxh32,
    produced: u64,
}

// SPEC(LZ4 Frame Format 1.6.4): magic, FLG, BD, optional content size, HC, blocks, end mark, optional checksum.
pub(crate) struct Lz4Frames<R> {
    input: R,
    frame: Option<Frame>,
    frames: u64,
    window: Vec<u8>,
    position: usize,
    finished: bool,
}

impl<R: BufRead> Lz4Frames<R> {
    pub(crate) fn new(input: R) -> Self {
        Lz4Frames {
            input,
            frame: None,
            frames: 0,
            window: Vec::new(),
            position: 0,
            finished: false,
        }
    }

    fn exact<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut bytes = [0u8; N];
        self.input
            .read_exact(&mut bytes)
            .map_err(|_| data_error("truncated LZ4 frame"))?;
        Ok(bytes)
    }

    fn word(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.exact::<4>()?))
    }

    fn start_frame(&mut self) -> io::Result<bool> {
        loop {
            if self.input.fill_buf()?.is_empty() {
                return if self.frames == 0 {
                    Err(data_error("LZ4 input holds no frame"))
                } else {
                    Ok(false)
                };
            }
            let magic = self.word()?;
            if magic & SKIPPABLE_MASK == SKIPPABLE_MAGIC {
                let size = u64::from(self.word()?);
                let skipped = io::copy(&mut (&mut self.input).take(size), &mut io::sink())?;
                if skipped != size {
                    return Err(data_error("truncated LZ4 skippable frame"));
                }
                continue;
            }
            if magic == LEGACY_MAGIC {
                return Err(data_error(
                    "LZ4 legacy frames, as `lz4 -l` writes, are not supported",
                ));
            }
            if magic != FRAME_MAGIC {
                return Err(data_error("LZ4 has data after its last frame"));
            }
            let [flags, block] = self.exact::<2>()?;
            if flags >> 6 != 1 || flags & 0x02 != 0 || block & 0x8F != 0 {
                return Err(data_error(
                    "LZ4 frame header has an unknown version or reserved bits",
                ));
            }
            if flags & 0x01 != 0 {
                return Err(data_error(
                    "LZ4 frame needs a dictionary, which is not supported",
                ));
            }
            let block_limit = match block >> 4 {
                4 => 64 * 1024,
                5 => 256 * 1024,
                6 => 1024 * 1024,
                7 => LARGEST_BLOCK,
                _ => return Err(data_error("LZ4 frame has an unknown block size")),
            };
            let mut descriptor = vec![flags, block];
            let content_size = if flags & 0x08 != 0 {
                let size = self.exact::<8>()?;
                descriptor.extend_from_slice(&size);
                Some(u64::from_le_bytes(size))
            } else {
                None
            };
            let [check] = self.exact::<1>()?;
            if check != (xxh32(&descriptor) >> 8) as u8 {
                return Err(data_error("LZ4 frame header checksum does not match"));
            }
            self.frames += 1;
            self.window.clear();
            self.position = 0;
            self.frame = Some(Frame {
                independent: flags & 0x20 != 0,
                block_checksums: flags & 0x10 != 0,
                content_checksum: flags & 0x04 != 0,
                content_size,
                block_limit,
                hash: Xxh32::new(),
                produced: 0,
            });
            return Ok(true);
        }
    }

    fn next_block(&mut self) -> io::Result<()> {
        let size = self.word()?;
        let mut frame = self.frame.take().expect("a frame");
        if size == 0 {
            if frame.content_checksum && self.word()? != frame.hash.digest() {
                return Err(data_error("LZ4 content checksum does not match"));
            }
            if frame
                .content_size
                .is_some_and(|size| size != frame.produced)
            {
                return Err(data_error("LZ4 frame does not decode to its content size"));
            }
            return Ok(());
        }
        let stored = size & !UNCOMPRESSED_BLOCK;
        if stored as usize > frame.block_limit {
            return Err(data_error("LZ4 block is larger than its frame allows"));
        }
        let mut data = vec![0u8; stored as usize];
        self.input
            .read_exact(&mut data)
            .map_err(|_| data_error("truncated LZ4 block"))?;
        if frame.block_checksums && self.word()? != xxh32(&data) {
            return Err(data_error("LZ4 block checksum does not match"));
        }
        if frame.independent {
            self.window.clear();
        } else if self.window.len() > HISTORY_HELD {
            self.window.drain(..self.window.len() - WINDOW);
        }
        let keep = self.window.len();
        let left = frame
            .content_size
            .map_or(u64::MAX, |size| size - frame.produced);
        let limit =
            usize::try_from(left).map_or(frame.block_limit, |left| left.min(frame.block_limit));
        if size & UNCOMPRESSED_BLOCK != 0 {
            if data.len() > limit {
                return Err(data_error("LZ4 frame decodes past its content size"));
            }
            self.window.extend_from_slice(&data);
        } else {
            decode_block(&data, &mut self.window, limit).ok_or_else(|| {
                data_error("LZ4 block is malformed or passes its frame's content size")
            })?;
        }
        frame.hash.update(&self.window[keep..]);
        frame.produced += (self.window.len() - keep) as u64;
        self.position = keep;
        self.frame = Some(frame);
        Ok(())
    }
}

impl<R: BufRead> Read for Lz4Frames<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while self.position == self.window.len() && !buffer.is_empty() {
            if self.finished {
                return Ok(0);
            }
            if self.frame.is_none() && !self.start_frame()? {
                self.finished = true;
                return Ok(0);
            }
            self.next_block()?;
        }
        let count = buffer.len().min(self.window.len() - self.position);
        buffer[..count].copy_from_slice(&self.window[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_block(block: &[u8], history: &[u8], limit: usize) -> Option<Vec<u8>> {
        let mut out = history.to_vec();
        let mut at = 0;
        let read_length = |at: &mut usize, mut length: usize| -> Option<usize> {
            if length == 15 {
                loop {
                    let next = *block.get(*at)?;
                    *at += 1;
                    length = length.checked_add(next as usize)?;
                    if next != 255 {
                        break;
                    }
                }
            }
            Some(length)
        };
        loop {
            let token = *block.get(at)?;
            at += 1;
            let literals = read_length(&mut at, (token >> 4) as usize)?;
            out.extend_from_slice(block.get(at..at.checked_add(literals)?)?);
            at += literals;
            if at == block.len() {
                return (out.len() - history.len() <= limit).then_some(out);
            }
            let offset = *block.get(at)? as usize | (*block.get(at + 1)? as usize) << 8;
            at += 2;
            let length = read_length(&mut at, (token & 15) as usize)? + 4;
            if offset == 0 || offset > out.len() || out.len() - history.len() + length > limit {
                return None;
            }
            for _ in 0..length {
                out.push(out[out.len() - offset]);
            }
        }
    }

    #[test]
    fn every_short_block_decodes_as_the_reference_does() {
        let alphabet = [0x00, 0x01, 0x04, 0x10, 0x1F, 0x41, 0xF0, 0xFF];
        let mut blocks = vec![Vec::new()];
        let mut checked = 0;
        for _ in 0..5 {
            blocks = blocks
                .iter()
                .flat_map(|block: &Vec<u8>| {
                    alphabet.iter().map(move |&byte| {
                        let mut next = block.clone();
                        next.push(byte);
                        next
                    })
                })
                .collect();
            for block in &blocks {
                for history in [&b""[..], b"a", b"abc"] {
                    for limit in [3, 64] {
                        let mut output = history.to_vec();
                        let decoded = decode_block(block, &mut output, limit).map(|()| output);
                        assert_eq!(
                            decoded,
                            reference_block(block, history, limit),
                            "{block:02x?} after {history:?} within {limit}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 200_000, "{checked}");
    }

    #[test]
    fn a_long_match_is_refused_before_the_output_grows_past_its_limit() {
        let mut block = vec![0x1F, b'a', 1, 0];
        block.extend(std::iter::repeat_n(255, 4000));
        block.extend_from_slice(&[0, 0x00]);
        let mut output = Vec::new();
        assert!(decode_block(&block, &mut output, 1024).is_none());
        assert!(output.len() <= 1024, "grew to {}", output.len());
    }

    #[test]
    fn xxh32_matches_known_values_and_every_split() {
        assert_eq!(xxh32(b""), 0x02CC_5D05);
        assert_eq!(xxh32(b"abc"), 0x32D1_53FF);
        let data: Vec<u8> = (0..70u8).map(|byte| byte.wrapping_mul(37)).collect();
        for length in 0..data.len() {
            let whole = xxh32(&data[..length]);
            for split in 0..=length {
                for second in split..=length {
                    let mut hash = Xxh32::new();
                    hash.update(&data[..split]);
                    hash.update(&data[split..second]);
                    hash.update(&data[second..length]);
                    assert_eq!(hash.digest(), whole, "{length} split {split} {second}");
                }
            }
        }
    }

    fn encode_block(data: &[u8]) -> Vec<u8> {
        let mut block = Vec::new();
        let mut at = 0;
        while at < data.len() {
            let run = data[at..]
                .iter()
                .take_while(|&&byte| byte == data[at])
                .count();
            if run >= 5 && data.len() - at - run >= 1 {
                let length = run - 1 - 4;
                block.push(0x10 | length.min(15) as u8);
                if length >= 15 {
                    let mut rest = length - 15;
                    while rest >= 255 {
                        block.push(255);
                        rest -= 255;
                    }
                    block.push(rest as u8);
                }
                let token = block.len()
                    - if length >= 15 {
                        2 + (length - 15) / 255
                    } else {
                        1
                    };
                block.insert(token + 1, data[at]);
                block.extend_from_slice(&1u16.to_le_bytes());
                at += run;
            } else {
                let literals = &data[at..];
                let token = (literals.len().min(15) as u8) << 4;
                block.push(token);
                if literals.len() >= 15 {
                    let mut rest = literals.len() - 15;
                    while rest >= 255 {
                        block.push(255);
                        rest -= 255;
                    }
                    block.push(rest as u8);
                }
                block.extend_from_slice(literals);
                return block;
            }
        }
        block.push(0);
        block
    }

    fn frame(blocks: &[Vec<u8>], flags: u8, content_size: Option<u64>) -> Vec<u8> {
        let mut frame = FRAME_MAGIC.to_le_bytes().to_vec();
        let flags = flags | 0x40 | if content_size.is_some() { 0x08 } else { 0 };
        let mut descriptor = vec![flags, 0x40];
        if let Some(size) = content_size {
            descriptor.extend_from_slice(&size.to_le_bytes());
        }
        frame.extend_from_slice(&descriptor);
        frame.push((xxh32(&descriptor) >> 8) as u8);
        let mut content = Vec::new();
        for data in blocks {
            let block = encode_block(data);
            frame.extend_from_slice(&(block.len() as u32).to_le_bytes());
            frame.extend_from_slice(&block);
            if flags & 0x10 != 0 {
                frame.extend_from_slice(&xxh32(&block).to_le_bytes());
            }
            content.extend_from_slice(data);
        }
        frame.extend_from_slice(&0u32.to_le_bytes());
        if flags & 0x04 != 0 {
            frame.extend_from_slice(&xxh32(&content).to_le_bytes());
        }
        frame
    }

    fn decode(bytes: &[u8]) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        Lz4Frames::new(bytes).read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn frames_round_trip_and_every_damage_is_refused() {
        let parts = [b"hello hello ".to_vec(), vec![b'z'; 700], b"tail".to_vec()];
        let content: Vec<u8> = parts.concat();
        for flags in [0x20, 0x20 | 0x10 | 0x04, 0x04] {
            let one = frame(&parts, flags, Some(content.len() as u64));
            assert_eq!(decode(&one).unwrap(), content, "flags {flags:02x}");
            let mut two = one.clone();
            two.extend_from_slice(&[0x5A, 0x2A, 0x4D, 0x18, 3, 0, 0, 0, 1, 2, 3]);
            two.extend_from_slice(&one);
            assert_eq!(decode(&two).unwrap(), [&content[..], &content[..]].concat());
            for length in 0..one.len() {
                assert!(
                    decode(&one[..length]).is_err(),
                    "cut to {length} of {}",
                    one.len()
                );
            }
            let mut trailing = one.clone();
            trailing.push(0);
            assert!(decode(&trailing).is_err());
            for at in 4..one.len() {
                let mut damaged = one.clone();
                damaged[at] ^= 0x01;
                if let Ok(decoded) = decode(&damaged)
                    && flags & 0x04 != 0
                {
                    assert_eq!(
                        decoded, content,
                        "flip at {at} decoded wrongly under a content checksum"
                    );
                }
            }
        }
        let wrong_size = frame(&parts, 0x20, Some(content.len() as u64 + 1));
        assert!(decode(&wrong_size).is_err());
    }

    fn raw_frame(flags: u8, content_size: Option<u64>, blocks: &[Vec<u8>]) -> Vec<u8> {
        let mut frame = FRAME_MAGIC.to_le_bytes().to_vec();
        let mut descriptor = vec![
            flags | 0x40 | if content_size.is_some() { 0x08 } else { 0 },
            0x70,
        ];
        if let Some(size) = content_size {
            descriptor.extend_from_slice(&size.to_le_bytes());
        }
        frame.extend_from_slice(&descriptor);
        frame.push((xxh32(&descriptor) >> 8) as u8);
        for block in blocks {
            frame.extend_from_slice(&(block.len() as u32).to_le_bytes());
            frame.extend_from_slice(block);
        }
        frame.extend_from_slice(&0u32.to_le_bytes());
        frame
    }

    #[test]
    fn linked_matches_reach_back_a_full_window_across_many_small_blocks() {
        let mut blocks = Vec::new();
        let mut expected = Vec::new();
        for index in 0..300_000u32 {
            let byte = (index.wrapping_mul(2_654_435_761) >> 24) as u8;
            blocks.push(vec![0x10, byte]);
            expected.push(byte);
        }
        blocks.push(vec![0x0F, 0xFF, 0xFF, 0xFF, 0xF1, 0x00]);
        let tail = expected.len() - 0xFFFF;
        let reached: Vec<u8> = (0..15 + 255 + 241 + 4)
            .map(|at| expected[tail + at])
            .collect();
        expected.extend_from_slice(&reached);
        assert_eq!(decode(&raw_frame(0, None, &blocks)).unwrap(), expected);
    }

    #[test]
    fn a_frame_that_passes_its_content_size_stops_at_the_size() {
        let mut big = vec![0x1F, b'z', 1, 0];
        big.extend(std::iter::repeat_n(255, 16_000));
        big.extend_from_slice(&[0, 0x00]);
        for (block, flags) in [
            (big, 0x20),
            (vec![0x70, b'a', b'b', b'c', b'd', b'e', b'f', b'g'], 0x20),
        ] {
            let frame = raw_frame(flags, Some(1), &[block]);
            let mut reader = Lz4Frames::new(&frame[..]);
            let mut buffer = [0u8; 4096];
            let mut produced = 0;
            let error = loop {
                match reader.read(&mut buffer) {
                    Ok(0) => panic!("a frame over its content size was accepted"),
                    Ok(read) => produced += read,
                    Err(error) => break error,
                }
            };
            assert!(produced <= 1, "{produced} bytes before {error}");
        }
    }

    #[test]
    fn linked_blocks_reach_back_into_the_previous_block() {
        let first = b"abcdefgh".to_vec();
        let mut linked = FRAME_MAGIC.to_le_bytes().to_vec();
        let descriptor = [0x40, 0x40];
        linked.extend_from_slice(&descriptor);
        linked.push((xxh32(&descriptor) >> 8) as u8);
        for block in [encode_block(&first), vec![0x04, 8, 0, 0x00]] {
            linked.extend_from_slice(&(block.len() as u32).to_le_bytes());
            linked.extend_from_slice(&block);
        }
        linked.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(decode(&linked).unwrap(), b"abcdefghabcdefgh");
        linked[4] |= 0x20;
        let header = [linked[4], linked[5]];
        linked[6] = (xxh32(&header) >> 8) as u8;
        assert!(
            decode(&linked).is_err(),
            "an independent block reached back"
        );
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(6)]
    fn decoding_any_short_block_never_panics_or_passes_its_limit() {
        let block: [u8; 4] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= block.len());
        let history: u8 = kani::any();
        let kept: bool = kani::any();
        let limit: usize = kani::any();
        kani::assume(limit <= 4);
        let mut output = Vec::with_capacity(8);
        if kept {
            output.push(history);
        }
        let start = output.len();
        if decode_block(&block[..length], &mut output, limit).is_some() {
            assert!(output.len() <= start + limit);
        }
    }
}
