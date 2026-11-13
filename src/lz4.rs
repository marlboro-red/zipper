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
const STRIPE: usize = 16;
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
    held: [u8; STRIPE],
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

    fn lanes_between_stripes(&self) -> Option<[u32; 4]> {
        (self.held_len == 0).then_some(self.lanes)
    }

    fn stripe_into(lanes: &mut [u32; 4], stripe: &[u8; STRIPE]) {
        let (words, _) = stripe.as_chunks::<4>();
        for (lane, word) in lanes.iter_mut().zip(words) {
            *lane = Self::round(*lane, u32::from_le_bytes(*word));
        }
    }

    fn add_stripes(&mut self, lanes: [u32; 4], stripes: &[u8]) {
        debug_assert!(self.held_len == 0 && stripes.len().is_multiple_of(STRIPE));
        self.lanes = lanes;
        self.total += stripes.len() as u64;
    }

    pub(crate) fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        if self.held_len > 0 {
            let take = (STRIPE - self.held_len).min(data.len());
            self.held[self.held_len..self.held_len + take].copy_from_slice(&data[..take]);
            self.held_len += take;
            data = &data[take..];
            if self.held_len < STRIPE {
                return;
            }
            let held = self.held;
            Self::stripe_into(&mut self.lanes, &held);
            self.held_len = 0;
        }
        let (stripes, rest) = data.as_chunks::<STRIPE>();
        for stripe in stripes {
            Self::stripe_into(&mut self.lanes, stripe);
        }
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
    decode_block_copying::<WILD>(block, output, limit)
}

fn decode_block_copying<const WILD: usize>(
    block: &[u8],
    output: &mut Vec<u8>,
    limit: usize,
) -> Option<()> {
    let start = output.len();
    let room = decoded_bound(block.len(), limit);
    output.resize(start.checked_add(room)?.checked_add(WILD)?, 0);
    let end = decode_into::<WILD>(block, output, start);
    output.truncate(end.unwrap_or(start));
    end.map(|_| ())
}

const WILD: usize = 16;
pub(crate) const DECODE_SPARE: usize = WILD;
const MOST_OUTPUT_PER_INPUT_BYTE: usize = 255;

fn decoded_bound(block: usize, limit: usize) -> usize {
    block.saturating_mul(MOST_OUTPUT_PER_INPUT_BYTE).min(limit)
}

#[derive(Clone, Copy)]
struct Resume {
    index: usize,
    out: usize,
}

enum Decoded {
    Ends(usize),
    Full(Resume),
}

fn decode_into<const WILD: usize>(block: &[u8], window: &mut [u8], start: usize) -> Option<usize> {
    let from = Resume {
        index: 0,
        out: start,
    };
    match decode_following::<WILD>(block, window, from, &mut |_, _| {})? {
        Decoded::Ends(end) => Some(end),
        Decoded::Full(_) => None,
    }
}

fn decode_following<const WILD: usize>(
    block: &[u8],
    window: &mut [u8],
    from: Resume,
    follow: &mut impl FnMut(&[u8], usize),
) -> Option<Decoded> {
    let end = window.len().checked_sub(WILD)?;
    let Resume { mut index, mut out } = from;
    if out > end {
        return None;
    }
    loop {
        follow(window, out);
        let at = Resume { index, out };
        let token = *block.get(index)?;
        if let Some(input) = block.get(index..index + WILD + 3)
            && end - out >= 3 * WILD
        {
            let literals = usize::from(token >> 4);
            let matched = usize::from(token & 0x0F) + MIN_MATCH;
            if literals < 15 && token & 0x0F < 15 && literals <= WILD && matched <= 2 * WILD {
                window[out..out + WILD].copy_from_slice(&input[1..1 + WILD]);
                out += literals;
                let offset = usize::from(u16::from_le_bytes([
                    input[1 + literals],
                    input[2 + literals],
                ]));
                index += 3 + literals;
                if offset >= WILD && offset <= out {
                    let from = out - offset;
                    window.copy_within(from..from + WILD, out);
                    if matched > WILD {
                        window.copy_within(from + WILD..from + 2 * WILD, out + WILD);
                    }
                } else if offset == 0 || offset > out {
                    return None;
                } else {
                    copy_match::<WILD>(window, out - offset, out, matched);
                }
                out += matched;
                continue;
            }
        }
        index += 1;
        let literals = length(block, &mut index, usize::from(token >> 4))?;
        if literals > block.len() - index {
            return None;
        }
        if literals > end - out {
            return Some(Decoded::Full(at));
        }
        if literals <= WILD && block.len() - index >= WILD {
            window[out..out + WILD].copy_from_slice(&block[index..index + WILD]);
        } else {
            window[out..out + literals].copy_from_slice(&block[index..index + literals]);
        }
        out += literals;
        index += literals;
        if index == block.len() {
            return Some(Decoded::Ends(out));
        }
        let offset = usize::from(u16::from_le_bytes([
            *block.get(index)?,
            *block.get(index + 1)?,
        ]));
        index += 2;
        let matched =
            length(block, &mut index, usize::from(token & 0x0F))?.checked_add(MIN_MATCH)?;
        if offset == 0 || offset > out {
            return None;
        }
        if matched > end - out {
            return Some(Decoded::Full(at));
        }
        copy_match::<WILD>(window, out - offset, out, matched);
        out += matched;
    }
}

fn copy_match<const WILD: usize>(window: &mut [u8], from: usize, to: usize, matched: usize) {
    let offset = to - from;
    if offset >= matched && matched > 2 * WILD {
        window.copy_within(from..from + matched, to);
    } else if offset >= WILD {
        let mut copied = 0;
        while copied < matched {
            window.copy_within(from + copied..from + copied + WILD, to + copied);
            copied += WILD;
        }
    } else {
        let mut copied = 0;
        while copied < matched {
            let pattern = (matched - copied).min(to + copied - from);
            window.copy_within(from..from + pattern, to + copied);
            copied += pattern;
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
    filled: usize,
    position: usize,
    block: Vec<u8>,
    finished: bool,
}

impl<R: BufRead> Lz4Frames<R> {
    pub(crate) fn new(input: R) -> Self {
        Lz4Frames {
            input,
            frame: None,
            frames: 0,
            window: Vec::new(),
            filled: 0,
            position: 0,
            block: Vec::new(),
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
            self.filled = 0;
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
        let stored = stored as usize;
        if self.block.len() < stored {
            self.block.resize(stored, 0);
        }
        self.input
            .read_exact(&mut self.block[..stored])
            .map_err(|_| data_error("truncated LZ4 block"))?;
        if frame.block_checksums && self.word()? != xxh32(&self.block[..stored]) {
            return Err(data_error("LZ4 block checksum does not match"));
        }
        if frame.independent {
            self.filled = 0;
        } else if self.filled > HISTORY_HELD
            || (self.filled > WINDOW && self.filled + first_room(stored) > self.window.len())
        {
            self.window
                .copy_within(self.filled - WINDOW..self.filled, 0);
            self.filled = WINDOW;
        }
        let keep = self.filled;
        self.position = keep;
        let left = frame
            .content_size
            .map_or(u64::MAX, |size| size - frame.produced);
        let limit =
            usize::try_from(left).map_or(frame.block_limit, |left| left.min(frame.block_limit));
        let data = &self.block[..stored];
        let mut hashed_to = keep;
        if size & UNCOMPRESSED_BLOCK != 0 {
            if stored > limit {
                return Err(data_error("LZ4 frame decodes past its content size"));
            }
            grow(&mut self.window, keep, keep + stored);
            self.window[keep..keep + stored].copy_from_slice(data);
            self.filled = keep + stored;
        } else {
            let most = keep + decoded_bound(stored, limit) + WILD;
            let lanes = frame
                .hash
                .lanes_between_stripes()
                .filter(|_| frame.content_checksum);
            let decoded = if let Some(mut lanes) = lanes {
                let decoded =
                    decode_growing(&mut self.window, data, keep, most, &mut |window, out| {
                        if let Some(stripe) = window[hashed_to..out].first_chunk::<STRIPE>() {
                            Xxh32::stripe_into(&mut lanes, stripe);
                            hashed_to += STRIPE;
                        }
                    });
                frame.hash.add_stripes(lanes, &self.window[keep..hashed_to]);
                decoded
            } else {
                decode_growing(&mut self.window, data, keep, most, &mut |_, _| {})
            };
            self.filled = decoded.ok_or_else(|| {
                data_error("LZ4 block is malformed or passes its frame's content size")
            })?;
        }
        if frame.content_checksum {
            frame.hash.update(&self.window[hashed_to..self.filled]);
        }
        frame.produced += (self.filled - keep) as u64;
        self.frame = Some(frame);
        Ok(())
    }
}

const FIRST_ROOM: usize = 64 * 1024;
const FIRST_ROOM_PER_INPUT_BYTE: usize = 4;
const ROOM_GROWTH: usize = 4;

fn first_room(stored: usize) -> usize {
    WILD + FIRST_ROOM.max(stored.saturating_mul(FIRST_ROOM_PER_INPUT_BYTE))
}

fn decode_growing(
    window: &mut Vec<u8>,
    block: &[u8],
    keep: usize,
    most: usize,
    follow: &mut impl FnMut(&[u8], usize),
) -> Option<usize> {
    let first = keep + first_room(block.len());
    let mut room = most.min(window.len().max(first));
    grow(window, keep, room);
    let mut from = Resume {
        index: 0,
        out: keep,
    };
    loop {
        match decode_following::<WILD>(block, &mut window[..room], from, follow)? {
            Decoded::Ends(end) => return Some(end),
            Decoded::Full(_) if room == most => return None,
            Decoded::Full(at) => {
                from = at;
                room = most.min(room.saturating_mul(ROOM_GROWTH));
                grow(window, at.out, room);
            }
        }
    }
}

fn grow(window: &mut Vec<u8>, keep: usize, len: usize) {
    if window.len() < len {
        let mut grown = vec![0; len];
        grown[..keep].copy_from_slice(&window[..keep]);
        *window = grown;
    }
}

impl<R: BufRead> Read for Lz4Frames<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while self.position == self.filled && !buffer.is_empty() {
            if self.finished {
                return Ok(0);
            }
            if self.frame.is_none() && !self.start_frame()? {
                self.finished = true;
                return Ok(0);
            }
            self.next_block()?;
        }
        let count = buffer.len().min(self.filled - self.position);
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
        let alphabet = [0x00, 0x01, 0x04, 0x10, 0x1F, 0x20, 0x41, 0xF0, 0xFF];
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
                for history in [&b""[..], b"a", b"abc", b"abcdefgh"] {
                    for limit in [3, 4, 5, 6, 64] {
                        for (wild, decoder) in [
                            (WILD, decode_block as fn(&[u8], &mut Vec<u8>, usize) -> _),
                            (1, decode_block_copying::<1>),
                            (2, decode_block_copying::<2>),
                        ] {
                            let mut output = history.to_vec();
                            let decoded = decoder(block, &mut output, limit).map(|()| output);
                            assert_eq!(
                                decoded,
                                reference_block(block, history, limit),
                                "{block:02x?} after {history:?} within {limit}, copying {wild}"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 3_000_000, "{checked}");
    }

    struct Random(u64);

    impl Random {
        fn below(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % bound as u64) as usize
        }

        fn length(&mut self, short: usize, long: usize) -> usize {
            if self.below(8) == 0 {
                self.below(long)
            } else {
                self.below(short)
            }
        }
    }

    fn push_length(block: &mut Vec<u8>, length: usize) {
        if length >= 15 {
            let mut rest = length - 15;
            while rest >= 255 {
                block.push(255);
                rest -= 255;
            }
            block.push(rest as u8);
        }
    }

    fn random_block(random: &mut Random, history: usize, long_match: usize) -> Vec<u8> {
        let mut block = Vec::new();
        let mut produced = history;
        for _ in 0..random.below(12) {
            let literals = random.length(20, 600);
            let reach = (produced + literals).min(usize::from(u16::MAX));
            if reach == 0 {
                continue;
            }
            let offset = 1 + if random.below(2) == 0 {
                random.below(reach.min(20))
            } else {
                random.below(reach)
            };
            let matched = random.length(24, long_match);
            block.push((literals.min(15) as u8) << 4 | matched.min(15) as u8);
            push_length(&mut block, literals);
            block.extend((0..literals).map(|_| random.below(256) as u8));
            block.extend_from_slice(&(offset as u16).to_le_bytes());
            push_length(&mut block, matched);
            produced += literals + matched + MIN_MATCH;
        }
        let literals = random.length(20, 300);
        block.push((literals.min(15) as u8) << 4);
        push_length(&mut block, literals);
        block.extend((0..literals).map(|_| random.below(256) as u8));
        block
    }

    #[test]
    fn random_and_damaged_blocks_decode_as_the_reference_does() {
        let mut random = Random(0x9E37_79B9_7F4A_7C15);
        for round in 0..20_000 {
            let history: Vec<u8> = (0..random.length(40, 70_000))
                .map(|_| random.below(256) as u8)
                .collect();
            let mut block = random_block(&mut random, history.len(), 2000);
            let whole = reference_block(&block, &history, usize::MAX)
                .expect("a generated block is well formed")
                .len()
                - history.len();
            if round % 3 == 1 {
                for _ in 0..1 + random.below(3) {
                    let at = random.below(block.len());
                    block[at] = random.below(256) as u8;
                }
            }
            if round % 7 == 2 {
                block.truncate(random.below(block.len() + 1));
            }
            for limit in [whole, whole.saturating_sub(1), whole + 1, whole + 100] {
                let expected = reference_block(&block, &history, limit);
                for decoder in [
                    decode_block as fn(&[u8], &mut Vec<u8>, usize) -> _,
                    decode_block_copying::<2>,
                ] {
                    let mut output = history.clone();
                    let decoded = decoder(&block, &mut output, limit).map(|()| output);
                    assert!(
                        decoded == expected,
                        "round {round} within {limit} after {} bytes",
                        history.len()
                    );
                }
            }
        }
    }

    #[test]
    fn blocks_that_outgrow_their_first_room_decode_as_the_reference_does() {
        let mut random = Random(0xD1B5_4A32_D192_ED03);
        for flags in [0x04, 0x20 | 0x04, 0x00] {
            let descriptor = [0x40 | flags, 0x70];
            let mut frame = FRAME_MAGIC.to_le_bytes().to_vec();
            frame.extend_from_slice(&descriptor);
            frame.push((xxh32(&descriptor) >> 8) as u8);
            let mut content = Vec::new();
            for _ in 0..40 {
                let history = if flags & 0x20 == 0 {
                    &content[content.len().saturating_sub(WINDOW)..]
                } else {
                    &[][..]
                };
                let block = random_block(&mut random, history.len(), 200_000);
                let decoded = reference_block(&block, history, LARGEST_BLOCK)
                    .expect("a generated block is well formed")[history.len()..]
                    .to_vec();
                frame.extend_from_slice(&(block.len() as u32).to_le_bytes());
                frame.extend_from_slice(&block);
                content.extend_from_slice(&decoded);
            }
            frame.extend_from_slice(&0u32.to_le_bytes());
            if flags & 0x04 != 0 {
                frame.extend_from_slice(&xxh32(&content).to_le_bytes());
            }
            assert!(decode(&frame).unwrap() == content, "flags {flags:02x}");
        }
    }

    #[test]
    fn a_block_at_its_largest_expansion_decodes() {
        let mut block = vec![0x1F, b'a', 1, 0];
        block.extend(std::iter::repeat_n(255, 1300));
        block.extend_from_slice(&[254, 0x00]);
        let expected = 1 + 15 + MIN_MATCH + 255 * 1300 + 254;
        assert!(expected > (MOST_OUTPUT_PER_INPUT_BYTE - 1) * block.len());
        let mut output = Vec::new();
        assert_eq!(decode_block(&block, &mut output, expected), Some(()));
        assert_eq!(output, vec![b'a'; expected]);
    }

    #[test]
    fn a_start_past_the_room_is_refused() {
        for start in [5, 10, 19] {
            assert_eq!(decode_into::<WILD>(&[0x00], &mut [0; 20], start), None);
        }
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
                block.push(data[at]);
                block.extend_from_slice(&1u16.to_le_bytes());
                push_length(&mut block, length);
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

    #[test]
    fn content_checksums_hold_over_blocks_of_every_alignment() {
        let mut random = Random(0x2545_F491_4F6C_DD1D);
        for lengths in [
            &[16, 32, 4096][..],
            &[5, 16, 48, 1000],
            &[17, 15, 1024, 1, 4095, 64],
            &[0, 160, 3],
        ] {
            let parts: Vec<Vec<u8>> = lengths
                .iter()
                .map(|&length| {
                    let mut part = Vec::new();
                    while part.len() < length {
                        let byte = random.below(4) as u8;
                        let run = 1 + random.length(3, 40);
                        part.extend(std::iter::repeat_n(byte, run.min(length - part.len())));
                    }
                    part
                })
                .collect();
            let content = parts.concat();
            for flags in [0x20 | 0x04, 0x04] {
                let whole = frame(&parts, flags, None);
                assert_eq!(decode(&whole).unwrap(), content, "{lengths:?}");
                let checksum = whole.len() - 4;
                let mut damaged = whole.clone();
                damaged[checksum] ^= 0x01;
                assert!(decode(&damaged).is_err(), "{lengths:?}");
            }
        }
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
    fn reading_on_after_a_malformed_block_never_panics() {
        let first = vec![0x80, b'a', b'b', b'c', b'd', b'e', b'f', b'g', b'h'];
        for flags in [0x20, 0x00] {
            let frame = raw_frame(flags, None, &[first.clone(), vec![0x00, 0x00]]);
            let mut reader = Lz4Frames::new(&frame[..]);
            let mut buffer = [0u8; 64];
            assert_eq!(reader.read(&mut buffer).unwrap(), 8);
            assert!(reader.read(&mut buffer).is_err());
            for _ in 0..3 {
                let _ = reader.read(&mut buffer);
            }
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
        let start = usize::from(kani::any::<bool>());
        let limit: usize = kani::any();
        kani::assume(limit <= 4);
        let mut window = [0u8; 1 + 4 + WILD];
        let size = start + decoded_bound(length, limit) + WILD;
        if let Some(end) = decode_into::<WILD>(&block[..length], &mut window[..size], start) {
            assert!(start <= end && end <= start + limit);
        }
    }

    #[kani::proof]
    #[kani::unwind(9)]
    fn short_copies_never_panic_touch_the_history_or_pass_the_room() {
        const NARROW: usize = 2;
        let block: [u8; 5] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= block.len());
        let history = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut window = history;
        let start: usize = kani::any();
        kani::assume(start <= window.len());
        if let Some(end) = decode_into::<NARROW>(&block[..length], &mut window, start) {
            assert!(start <= end && end + NARROW <= window.len());
        }
        assert!(window[..start] == history[..start]);
    }
}
