//! Zipper's decoder against libzstd 1.5.7, as a stream and in one call. Both
//! must give the same bytes or both refuse. Two differences are allowed, both
//! in a stream. Zipper refuses a match from beyond the window, which libzstd
//! copies from whatever its buffer holds, and a content size that lies before
//! an empty last block, which libzstd does not check there.

use super::*;
use std::io::Write;

pub(super) fn ours(frames: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoded = Vec::new();
    match ZstdReader::new(frames).read_to_end(&mut decoded) {
        Ok(_) => Ok(decoded),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn libzstd(frames: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoded = Vec::new();
    let mut reader = zstd::stream::read::Decoder::with_buffer(frames).unwrap();
    match reader.read_to_end(&mut decoded) {
        Ok(_) => Ok(decoded),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn encode(
    data: &[u8],
    level: i32,
    setup: impl Fn(&mut zstd::Encoder<Vec<u8>>),
) -> Vec<u8> {
    let mut encoder = zstd::Encoder::new(Vec::new(), level).unwrap();
    setup(&mut encoder);
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

pub(super) struct Random(pub u64);

impl Random {
    pub(super) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub(super) fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit as u64) as usize
    }
}

pub(super) fn sources() -> Vec<u8> {
    let mut text = Vec::new();
    let mut paths: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    for path in paths {
        if path.is_file() {
            text.extend(std::fs::read(path).unwrap());
        }
    }
    text
}

pub(super) fn mixed(random: &mut Random, text: &[u8], length: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(length);
    while data.len() < length {
        let run = 1 + random.below(3000);
        match random.below(5) {
            0 => data.extend((0..run).map(|_| random.next() as u8)),
            1 => data.extend(std::iter::repeat_n(random.next() as u8, run)),
            2 if !data.is_empty() => {
                let back = 1 + random.below(data.len().min(70_000));
                for _ in 0..run {
                    data.push(data[data.len() - back]);
                }
            }
            3 => data.extend((0..run).map(|_| b"abcdefgh"[random.below(3)])),
            _ => {
                let from = random.below(text.len() - run);
                data.extend_from_slice(&text[from..from + run]);
            }
        }
    }
    data.truncate(length);
    data
}

#[test]
fn xxh64_matches_its_published_values() {
    let hash = |data: &[u8]| {
        let mut hash = Xxh64::new();
        hash.update(data);
        hash.digest()
    };
    assert_eq!(hash(b""), 0xEF46_DB37_51D8_E999);
    assert_eq!(hash(b"abc"), 0x44BC_2CF5_AD77_0999);
}

#[test]
fn frames_at_every_level_decode_as_libzstd_decodes_them() {
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let text = sources();
    let inputs = [
        Vec::new(),
        b"a".to_vec(),
        text[..1000].to_vec(),
        text[..300_000].to_vec(),
        text.clone(),
        mixed(&mut random, &text, 700_000),
        (0..200_000).map(|_| random.next() as u8).collect(),
        vec![7u8; 500_000],
    ];
    for (number, input) in inputs.iter().enumerate() {
        for level in (-5..=22).filter(|level| *level != 0) {
            if level > 19 && input.len() > 400_000 {
                continue;
            }
            let frame = encode(input, level, |encoder| {
                encoder.include_checksum(level % 2 == 0).unwrap();
            });
            let decoded = ours(&frame);
            assert!(
                decoded.as_deref() == Ok(&input[..]),
                "input {number} level {level}: {:?}",
                decoded.as_ref().err()
            );
        }
    }
}

pub(super) fn skewed(random: &mut Random, length: usize) -> Vec<u8> {
    (0..length)
        .map(|_| match random.below(100) {
            0..70 => b'a',
            70..85 => b'b',
            85..93 => b'c',
            93..97 => b'd',
            _ => b'e' + random.below(20) as u8,
        })
        .collect()
}

fn ours_one_shot(frames: &[u8], capacity: usize) -> Result<Vec<u8>, String> {
    let mut output = vec![0u8; capacity];
    match decode_frames_into(frames, &mut output) {
        Ok(length) => {
            output.truncate(length);
            Ok(output)
        }
        Err(error) => Err(error.0.to_owned()),
    }
}

fn libzstd_one_shot(frames: &[u8], capacity: usize) -> Result<Vec<u8>, String> {
    let mut output = vec![0u8; capacity];
    let mut decoder = zstd::bulk::Decompressor::new().unwrap();
    match decoder.decompress_to_buffer(frames, &mut output[..]) {
        Ok(length) => {
            output.truncate(length);
            Ok(output)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn same_outcome(frames: &[u8], capacity: usize, what: &dyn Fn() -> String) {
    let streamed = ours(frames);
    let theirs = libzstd(frames);
    let stricter = streamed
        .as_ref()
        .is_err_and(|error| error.contains(BEYOND_WINDOW) || error.contains(SIZE_DIFFERS));
    if !(stricter && theirs.is_ok()) {
        assert!(
            streamed.is_err() == theirs.is_err() && (streamed.is_err() || streamed == theirs),
            "{}: streamed {:?} against libzstd {:?}",
            what(),
            streamed.as_ref().map(Vec::len),
            theirs.as_ref().map(Vec::len)
        );
    }
    let libzstd_streaming_refuses = theirs.is_err();
    let whole = ours_one_shot(frames, capacity);
    let theirs = libzstd_one_shot(frames, capacity);
    let oversized = whole.as_ref().is_err_and(|error| error.contains(PAST_SIZE));
    if oversized && libzstd_streaming_refuses {
        return;
    }
    assert!(
        whole.is_err() == theirs.is_err() && (whole.is_err() || whole == theirs),
        "{}: one shot {:?} against libzstd {:?}",
        what(),
        whole.as_ref().map(Vec::len),
        theirs.as_ref().map(Vec::len)
    );
}

fn shapes() -> Vec<(String, Vec<u8>, usize)> {
    let mut random = Random(0x2545_F491_4F6C_DD1D);
    let text = sources();
    let mut shapes = Vec::new();
    let mut add = |name: &str, frame: Vec<u8>, decoded: usize| {
        shapes.push((name.to_owned(), frame, decoded));
    };
    for (level, length) in [(1, 6_000), (3, 3_000), (9, 9_000), (19, 5_000), (22, 2_500)] {
        let input = &text[40_000..40_000 + length];
        add(
            &format!("text level {level}"),
            encode(input, level, |encoder| {
                encoder.include_checksum(level == 3).unwrap();
            }),
            length,
        );
    }
    let skew = skewed(&mut random, 9_000);
    add("skewed literals", encode(&skew, 3, |_| {}), skew.len());
    let small = &text[1000..1200];
    add("one stream", encode(small, 3, |_| {}), small.len());
    let noise: Vec<u8> = (0..700).map(|_| random.next() as u8).collect();
    add("raw block", encode(&noise, 3, |_| {}), noise.len());
    add("rle block", encode(&[9u8; 5000], 3, |_| {}), 5000);
    let mix = mixed(&mut random, &text, 12_000);
    add("mixed", encode(&mix, 5, |_| {}), mix.len());
    let mut blocks = zstd::Encoder::new(Vec::new(), 3).unwrap();
    let mut total = 0;
    for part in 0..6 {
        let piece = &text[part * 5000..part * 5000 + 1500 + part * 100];
        blocks.write_all(piece).unwrap();
        blocks.flush().unwrap();
        total += piece.len();
    }
    add("six flushed blocks", blocks.finish().unwrap(), total);
    let mut skews = zstd::Encoder::new(Vec::new(), 3).unwrap();
    for _ in 0..3 {
        skews.write_all(&skewed(&mut random, 4000)).unwrap();
        skews.flush().unwrap();
    }
    add("treeless skewed blocks", skews.finish().unwrap(), 12_000);
    let mut two = encode(&text[..2000], 3, |_| {});
    two.extend([0x53, 0x2A, 0x4D, 0x18, 3, 0, 0, 0, 1, 2, 3]);
    two.extend(encode(&text[2000..3000], 7, |encoder| {
        encoder.include_checksum(true).unwrap();
    }));
    add("two frames around a skippable frame", two, 3000);
    add(
        "small window",
        encode(&text[..40_000], 3, |encoder| {
            encoder.window_log(10).unwrap();
        }),
        40_000,
    );
    shapes
}

#[test]
fn every_cut_frame_is_refused_as_libzstd_refuses_it() {
    for (name, frame, decoded) in shapes() {
        for length in 0..frame.len() {
            same_outcome(&frame[..length], decoded, &|| {
                format!("{name} cut to {length}")
            });
        }
        assert_eq!(ours(&frame).map(|bytes| bytes.len()), Ok(decoded), "{name}");
    }
}

const FLIP_PARTS: usize = 3;

fn every_flip_of_every_shape(shapes: &[(String, Vec<u8>, usize)]) -> Vec<(usize, usize)> {
    shapes
        .iter()
        .enumerate()
        .flat_map(|(shape, (_, frame, _))| (0..frame.len() * 8).map(move |bit| (shape, bit)))
        .collect()
}

fn flip_part(flips: usize, part: usize) -> impl Iterator<Item = usize> {
    (part..flips).step_by(FLIP_PARTS)
}

fn flipped_bits_decode_or_are_refused_as_libzstd_does(part: usize) {
    let shapes = shapes();
    let flips = every_flip_of_every_shape(&shapes);
    for index in flip_part(flips.len(), part) {
        let (shape, bit) = flips[index];
        let (name, frame, decoded) = &shapes[shape];
        let mut damaged = frame.clone();
        damaged[bit / 8] ^= 1 << (bit % 8);
        same_outcome(&damaged, decoded + 4096, &|| format!("{name} bit {bit}"));
    }
}

#[test]
fn the_flip_parts_hold_every_flip_once() {
    let flips = every_flip_of_every_shape(&shapes()).len();
    let mut held: Vec<usize> = (0..FLIP_PARTS)
        .flat_map(|part| flip_part(flips, part))
        .collect();
    held.sort_unstable();
    assert_eq!(held, (0..flips).collect::<Vec<_>>());
}

#[test]
fn every_third_flip_from_the_first_decodes_or_is_refused_as_libzstd_does() {
    flipped_bits_decode_or_are_refused_as_libzstd_does(0);
}

#[test]
fn every_third_flip_from_the_second_decodes_or_is_refused_as_libzstd_does() {
    flipped_bits_decode_or_are_refused_as_libzstd_does(1);
}

#[test]
fn every_third_flip_from_the_third_decodes_or_is_refused_as_libzstd_does() {
    flipped_bits_decode_or_are_refused_as_libzstd_does(2);
}

#[test]
fn random_damage_decodes_or_is_refused_as_libzstd_does() {
    let mut random = Random(0x1234_5678_9ABC_DEF1);
    for (name, frame, decoded) in shapes() {
        for round in 0..3000 {
            let mut damaged = frame.clone();
            for _ in 0..1 + random.below(3) {
                let at = random.below(damaged.len());
                match random.below(4) {
                    0 => damaged[at] = random.next() as u8,
                    1 => damaged[at] = damaged[at].wrapping_add(1),
                    2 => {
                        damaged.remove(at);
                    }
                    _ => damaged.insert(at, random.next() as u8),
                }
            }
            same_outcome(&damaged, decoded + 4096, &|| {
                format!("{name} round {round}")
            });
        }
    }
}

#[test]
#[ignore = "a probe: ZSTD_PROBE=<file> ZSTD_PROBE_DECODER=zipper|libzstd ZSTD_PROBE_ROUNDS=<n>, counted by /usr/bin/time -l"]
fn decode_loop_for_counters() {
    let path = std::env::var("ZSTD_PROBE").expect("ZSTD_PROBE names a .zst file");
    let rounds: usize = std::env::var("ZSTD_PROBE_ROUNDS")
        .map(|rounds| rounds.parse().unwrap())
        .unwrap_or(1);
    let decoder = std::env::var("ZSTD_PROBE_DECODER").unwrap_or_default();
    let frames = std::fs::read(path).unwrap();
    let mut chunk = vec![0u8; 128 * 1024];
    let mut total = 0u64;
    for _ in 0..rounds {
        let mut reader: Box<dyn Read> = match decoder.as_str() {
            "zipper" => Box::new(ZstdReader::new(&frames[..])),
            "libzstd" => Box::new(zstd::stream::read::Decoder::with_buffer(&frames[..]).unwrap()),
            _ => Box::new(std::io::empty()),
        };
        loop {
            let read = reader.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            total += read as u64;
        }
    }
    eprintln!("PROBE {decoder} decoded {total} bytes in {rounds} rounds");
}

#[derive(Default, Clone)]
struct BackwardBitstream {
    fields: Vec<(u64, u32)>,
}

impl BackwardBitstream {
    fn push(&mut self, value: u64, bits: u32) {
        self.fields.push((value, bits));
    }

    fn bytes(&self) -> Vec<u8> {
        let total: usize = self.fields.iter().map(|field| field.1 as usize).sum();
        let mut bytes = vec![0u8; total / 8 + 1];
        bytes[total / 8] |= 1 << (total % 8);
        let mut at = total;
        for &(value, bits) in &self.fields {
            at -= bits as usize;
            for bit in 0..bits as usize {
                if value >> bit & 1 == 1 {
                    bytes[(at + bit) / 8] |= 1 << ((at + bit) % 8);
                }
            }
        }
        bytes
    }
}

struct TreeFromWeights {
    weights: Vec<u8>,
    codes: Vec<(u64, u32)>,
}

impl TreeFromWeights {
    fn new(weights: &[u8]) -> TreeFromWeights {
        let total: u32 = weights.iter().map(|&weight| (1u32 << weight) >> 1).sum();
        assert!(total.is_power_of_two(), "weights sum to {total}");
        let log = total.trailing_zeros();
        let mut codes = vec![(0, 0); weights.len()];
        let mut slot = 0u64;
        for weight in 1..=log {
            for (symbol, _) in weights
                .iter()
                .enumerate()
                .filter(|(_, held)| u32::from(**held) == weight)
            {
                codes[symbol] = (slot >> (weight - 1), log + 1 - weight);
                slot += 1 << (weight - 1);
            }
        }
        TreeFromWeights {
            weights: weights.to_vec(),
            codes,
        }
    }

    fn description(&self) -> Vec<u8> {
        let listed = &self.weights[..self.weights.len() - 1];
        let mut bytes = vec![127 + listed.len() as u8];
        for pair in listed.chunks(2) {
            bytes.push(pair[0] << 4 | pair.get(1).copied().unwrap_or(0));
        }
        bytes
    }

    fn stream(&self, symbols: &[u8]) -> Vec<u8> {
        let mut bits = BackwardBitstream::default();
        for &symbol in symbols {
            let (code, length) = self.codes[usize::from(symbol)];
            assert!(length > 0, "symbol {symbol} has no code");
            bits.push(code, length);
        }
        bits.bytes()
    }
}

fn plain_literals(kind: u8, format: u8, size: usize, payload: &[u8]) -> Vec<u8> {
    let mut section = match format {
        1 => ((size << 4) as u16 | 0b0100 | u16::from(kind))
            .to_le_bytes()
            .to_vec(),
        3 => ((size << 4) as u32 | 0b1100 | u32::from(kind)).to_le_bytes()[..3].to_vec(),
        _ => vec![(size << 3) as u8 | format << 2 | kind],
    };
    section.extend_from_slice(payload);
    section
}

fn raw_literals(format: u8, bytes: &[u8]) -> Vec<u8> {
    plain_literals(0, format, bytes.len(), bytes)
}

fn repeated_literals(format: u8, byte: u8, count: usize) -> Vec<u8> {
    plain_literals(1, format, count, &[byte])
}

fn coded_literals(kind: u8, format: u8, decoded: usize, payload: &[u8]) -> Vec<u8> {
    let (regenerated, encoded) = (decoded as u64, payload.len() as u64);
    let head = u64::from(kind) | u64::from(format) << 2 | regenerated << 4;
    let (header, length) = match format {
        0 | 1 => (head | encoded << 14, 3),
        2 => (head | encoded << 18, 4),
        _ => (head | encoded << 22, 5),
    };
    let mut section = header.to_le_bytes()[..length].to_vec();
    section.extend_from_slice(payload);
    section
}

fn huffman_literals(tree: &TreeFromWeights, treeless: bool, format: u8, symbols: &[u8]) -> Vec<u8> {
    let mut payload = if treeless {
        Vec::new()
    } else {
        tree.description()
    };
    if format == 0 {
        payload.extend(tree.stream(symbols));
    } else {
        let segment = symbols.len().div_ceil(4);
        let streams: Vec<Vec<u8>> = (0..4)
            .map(|stream| {
                let from = (stream * segment).min(symbols.len());
                let to = ((stream + 1) * segment).min(symbols.len());
                tree.stream(&symbols[from..to])
            })
            .collect();
        for stream in &streams[..3] {
            payload.extend((stream.len() as u16).to_le_bytes());
        }
        payload.extend(streams.concat());
    }
    coded_literals(
        if treeless { 3 } else { 2 },
        format,
        symbols.len(),
        &payload,
    )
}

fn sequence_count(count: usize) -> Vec<u8> {
    match count {
        0..0x80 => vec![count as u8],
        0x80..0x7F00 => vec![0x80 | (count >> 8) as u8, count as u8],
        _ => {
            let rest = (count - 0x7F00) as u16;
            vec![0xFF, rest as u8, (rest >> 8) as u8]
        }
    }
}

#[derive(Clone, Copy)]
struct OneCodePerField {
    literal: usize,
    offset: usize,
    matched: usize,
}

fn one_symbol_sequences(
    codes: OneCodePerField,
    repeat: bool,
    extras: &[(u64, u64, u64)],
) -> Vec<u8> {
    let mut section = sequence_count(extras.len());
    if repeat {
        section.push(0b1111_1100);
    } else {
        section.push(0b0101_0100);
        section.extend([codes.literal as u8, codes.offset as u8, codes.matched as u8]);
    }
    let mut bits = BackwardBitstream::default();
    for &(literal, offset, matched) in extras {
        bits.push(offset, u32::from(OFFSETS.extras[codes.offset]));
        bits.push(matched, u32::from(MATCH_LENGTHS.extras[codes.matched]));
        bits.push(literal, u32::from(LITERAL_LENGTHS.extras[codes.literal]));
    }
    section.extend(bits.bytes());
    section
}

fn block(kind: u8, last: bool, size: usize, payload: &[u8]) -> Vec<u8> {
    let header = (size as u32) << 3 | u32::from(kind) << 1 | u32::from(last);
    let mut bytes = header.to_le_bytes()[..3].to_vec();
    bytes.extend_from_slice(payload);
    bytes
}

fn compressed(last: bool, literals: &[u8], sequences: &[u8]) -> Vec<u8> {
    let payload = [literals, sequences].concat();
    block(2, last, payload.len(), &payload)
}

fn windowed(window_log: u32, blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut frame = MAGIC.to_le_bytes().to_vec();
    frame.extend([0, ((window_log - 10) << 3) as u8]);
    frame.extend(blocks.concat());
    frame
}

struct ByteAtATimeModel {
    output: Vec<u8>,
    repeats: [usize; 3],
}

impl ByteAtATimeModel {
    fn new() -> ByteAtATimeModel {
        ByteAtATimeModel {
            output: Vec::new(),
            repeats: [1, 4, 8],
        }
    }

    fn block(
        &mut self,
        literals: &[u8],
        codes: OneCodePerField,
        extras: &[(u64, u64, u64)],
    ) -> Option<()> {
        let mut used = 0;
        for &(literal, offset, matched) in extras {
            let count = LITERAL_LENGTHS.bases[codes.literal] as usize + literal as usize;
            let length = MATCH_LENGTHS.bases[codes.matched] as usize + matched as usize;
            let value = (1usize << codes.offset) + offset as usize;
            let offset = if value > 3 {
                self.repeats = [value - 3, self.repeats[0], self.repeats[1]];
                value - 3
            } else {
                let index = value - 1 + usize::from(count == 0);
                let picked = match index {
                    0 => self.repeats[0],
                    1 => self.repeats[1],
                    2 => self.repeats[2],
                    _ => self.repeats[0].checked_sub(1)?,
                };
                if index > 1 {
                    self.repeats[2] = self.repeats[1];
                }
                if index > 0 {
                    self.repeats[1] = self.repeats[0];
                }
                self.repeats[0] = picked;
                picked
            };
            self.output.extend(literals.get(used..used + count)?);
            used += count;
            if offset == 0 || offset > self.output.len() {
                return None;
            }
            for _ in 0..length {
                self.output.push(self.output[self.output.len() - offset]);
            }
        }
        self.output.extend(&literals[used..]);
        Some(())
    }
}

fn expect(frame: &[u8], decoded: Option<&[u8]>, what: &str) {
    let capacity = decoded.map_or(1 << 16, |bytes| bytes.len() + 64);
    same_outcome(frame, capacity, &|| what.to_owned());
    assert!(
        ours(frame).ok().as_deref() == decoded,
        "{what}: decoded {:?}, expected {:?} bytes",
        ours(frame).map(|bytes| bytes.len()),
        decoded.map(<[u8]>::len)
    );
    let whole = ours_one_shot(frame, capacity);
    assert!(
        whole.ok().as_deref() == decoded,
        "{what}: one shot differs from the model"
    );
}

fn every_flip(frame: &[u8], capacity: usize, what: &str) {
    let mut damaged = frame.to_vec();
    for bit in 0..frame.len() * 8 {
        damaged[bit / 8] ^= 1 << (bit % 8);
        same_outcome(&damaged, capacity, &|| format!("{what} bit {bit}"));
        damaged[bit / 8] ^= 1 << (bit % 8);
    }
    for length in 0..frame.len() {
        same_outcome(&frame[..length], capacity, &|| {
            format!("{what} cut to {length}")
        });
    }
}

fn trees() -> Vec<(&'static str, TreeFromWeights)> {
    let mut deep = vec![0u8; 40];
    deep[3] = 12;
    deep[9] = 11;
    deep[17] = 10;
    deep[20] = 9;
    deep[21] = 8;
    deep[22] = 7;
    deep[23] = 6;
    deep[24] = 5;
    deep[25] = 4;
    deep[30] = 3;
    deep[31] = 2;
    deep[38] = 1;
    deep[39] = 1;
    let mut wide = [[5u8; 8], [4; 8], [3; 8], [2; 8]].concat();
    wide.extend([1u8; 15]);
    wide.extend([0u8; 81]);
    wide.push(1);
    vec![
        ("two symbols", TreeFromWeights::new(&[1, 1])),
        ("three symbols", TreeFromWeights::new(&[2, 1, 1])),
        ("a gap", TreeFromWeights::new(&[1, 0, 0, 2, 0, 1])),
        ("eleven bits", TreeFromWeights::new(&deep[4..])),
        ("twelve bits", TreeFromWeights::new(&deep)),
        ("129 symbols", TreeFromWeights::new(&wide)),
    ]
}

fn spelled(tree: &TreeFromWeights, random: &mut Random, count: usize) -> Vec<u8> {
    let used: Vec<u8> = (0..tree.weights.len())
        .filter(|&symbol| tree.weights[symbol] > 0)
        .map(|symbol| symbol as u8)
        .collect();
    (0..count)
        .map(|_| {
            let pick = random.below(used.len() * 3);
            used[pick.min(used.len() - 1 - pick % 2)]
        })
        .collect()
}

#[test]
fn hand_built_literal_sections_decode_as_spelled_and_as_libzstd_decodes_them() {
    let mut random = Random(0x0DDB_1A5E_5BAD_5EED);
    for (name, tree) in trees() {
        for count in [
            0usize, 1, 2, 5, 6, 7, 9, 31, 64, 255, 900, 1023, 3000, 20_000,
        ] {
            let symbols = spelled(&tree, &mut random, count);
            for format in 0..4u8 {
                let fits = match format {
                    0 | 1 => count < 1024,
                    2 => count < 16384,
                    _ => true,
                };
                let literals = huffman_literals(&tree, false, format, &symbols);
                let payload = literals.len() - if format < 2 { 3 } else { format as usize + 2 };
                if !fits || (format < 2 && payload >= 1024) || (format == 2 && payload >= 16384) {
                    continue;
                }
                let what = format!("{name}, {count} literals, format {format}");
                let frame = windowed(17, &[compressed(true, &literals, &[0])]);
                let decoded = (format == 0 || count >= 6).then_some(&symbols[..]);
                expect(&frame, decoded, &what);
                if count <= 255 {
                    every_flip(&frame, count + 64, &what);
                }
                let again = huffman_literals(&tree, true, format, &symbols);
                let frame = windowed(
                    17,
                    &[
                        compressed(false, &literals, &[0]),
                        compressed(true, &again, &[0]),
                    ],
                );
                let twice = [&symbols[..], &symbols[..]].concat();
                expect(
                    &frame,
                    decoded.map(|_| &twice[..]),
                    &format!("{what}, then treeless"),
                );
                if count <= 64 || count == 900 {
                    every_flip(&frame, 2 * count + 64, &format!("{what}, then treeless"));
                }
            }
        }
    }
}

#[test]
fn a_tree_built_for_four_streams_reads_a_later_single_stream_as_libzstd_reads_it() {
    let mut random = Random(0x5EED_0F00_D15C_0BAD);
    for (name, tree) in trees() {
        let many = spelled(&tree, &mut random, 30_000);
        let first = huffman_literals(&tree, false, 3, &many);
        for count in [0usize, 1, 2, 3, 4, 17, 300] {
            let few = spelled(&tree, &mut random, count);
            let one = huffman_literals(&tree, true, 0, &few);
            let four = huffman_literals(&tree, true, 1, &few);
            for (streams, section) in [(1, &one), (4, &four)] {
                let frame = windowed(
                    17,
                    &[
                        compressed(false, &first, &[0]),
                        compressed(true, section, &[0]),
                    ],
                );
                let what = format!("{name}, {count} treeless literals in {streams} streams");
                let decoded = [&many[..], &few[..]].concat();
                expect(
                    &frame,
                    (streams == 1 || count >= 6).then_some(&decoded[..]),
                    &what,
                );
                let mut damaged = frame.clone();
                let tail = frame.len() - section.len() - 3;
                for bit in tail * 8..frame.len() * 8 {
                    damaged[bit / 8] ^= 1 << (bit % 8);
                    same_outcome(&damaged, decoded.len() + 64, &|| {
                        format!("{what} bit {bit}")
                    });
                    damaged[bit / 8] ^= 1 << (bit % 8);
                }
            }
        }
    }
}

#[test]
fn raw_and_repeated_literals_of_every_header_size_decode_as_libzstd_decodes_them() {
    let mut random = Random(0xABCD_EF01_2345_6789);
    for count in [0usize, 1, 31, 32, 100, 4095, 4096, 70_000, 131_068, 131_069] {
        let bytes: Vec<u8> = (0..count).map(|_| random.next() as u8).collect();
        for format in [0u8, 1, 3] {
            let fits = match format {
                0 => count < 32,
                1 => count < 4096,
                _ => true,
            };
            if !fits {
                continue;
            }
            let what = format!("{count} literals, format {format}");
            let raw = windowed(17, &[compressed(true, &raw_literals(format, &bytes), &[0])]);
            expect(
                &raw,
                (raw.len() <= 6 + 3 + BLOCK_MAX).then_some(&bytes[..]),
                &format!("raw {what}"),
            );
            let repeated = windowed(
                17,
                &[compressed(
                    true,
                    &repeated_literals(format, 0x5A, count),
                    &[0],
                )],
            );
            let run = vec![0x5A; count];
            expect(&repeated, Some(&run[..]), &format!("run {what}"));
            if count <= 100 {
                every_flip(&raw, count + 64, &format!("raw {what}"));
                every_flip(&repeated, count + 64, &format!("run {what}"));
            }
        }
    }
}

fn history(random: &mut Random, length: usize, chunk: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
    let bytes: Vec<u8> = (0..length).map(|_| random.next() as u8).collect();
    let blocks = bytes
        .chunks(chunk)
        .map(|chunk| block(0, false, chunk.len(), chunk))
        .collect();
    (blocks, bytes)
}

#[test]
fn one_symbol_sequences_of_every_code_decode_as_the_byte_at_a_time_model() {
    let mut random = Random(0x0123_4567_89AB_CDEF);
    let (blocks, bytes) = history(&mut random, 300_000, 100_000);
    let literals: Vec<u8> = (0..70_000).map(|_| random.next() as u8).collect();
    for literal in 0..=LITERAL_LENGTHS.max_symbol {
        for matched in (0..=MATCH_LENGTHS.max_symbol).step_by(if literal % 5 == 0 { 1 } else { 7 })
        {
            for offset in [0usize, 1, 2, 3, 5, 9, 14, 17, 18] {
                let codes = OneCodePerField {
                    literal,
                    offset,
                    matched,
                };
                let all = |bits: u8| (1u64 << bits) - 1;
                let extras = [
                    (0, 0, 0),
                    (
                        all(LITERAL_LENGTHS.extras[literal]) % 9000,
                        all(OFFSETS.extras[offset]),
                        all(MATCH_LENGTHS.extras[matched]),
                    ),
                    (
                        (random.next() & all(LITERAL_LENGTHS.extras[literal])) % 9000,
                        random.next() & all(OFFSETS.extras[offset]),
                        random.next() & all(MATCH_LENGTHS.extras[matched]),
                    ),
                    (
                        0,
                        random.next() & all(OFFSETS.extras[offset]),
                        1 & all(MATCH_LENGTHS.extras[matched]),
                    ),
                ];
                for count in 1..=extras.len() {
                    let extras = &extras[..count];
                    let needed: usize = extras
                        .iter()
                        .map(|extra| LITERAL_LENGTHS.bases[literal] as usize + extra.0 as usize)
                        .sum();
                    let held = &literals[..(needed + 5).min(literals.len())];
                    let mut model = ByteAtATimeModel {
                        output: bytes.clone(),
                        repeats: [1, 4, 8],
                    };
                    let fits = model.block(held, codes, extras).is_some()
                        && model.output.len() - bytes.len() <= BLOCK_MAX;
                    let last = compressed(
                        true,
                        &raw_literals(3, held),
                        &one_symbol_sequences(codes, false, extras),
                    );
                    let frame = windowed(
                        19,
                        &[blocks.concat(), last]
                            .concat()
                            .chunks(usize::MAX)
                            .map(<[u8]>::to_vec)
                            .collect::<Vec<_>>(),
                    );
                    let what = format!("codes {literal}/{offset}/{matched}, {count} sequences");
                    expect(&frame, fits.then_some(&model.output[..]), &what);
                }
            }
        }
    }
}

#[test]
fn sequences_that_repeat_offsets_and_tables_decode_as_the_byte_at_a_time_model() {
    let mut random = Random(0xFEED_FACE_CAFE_BEEF);
    for round in 0..4000 {
        let mut model = ByteAtATimeModel::new();
        let mut blocks = Vec::new();
        let mut valid = true;
        let mut codes = OneCodePerField {
            literal: 0,
            offset: 0,
            matched: 0,
        };
        let count = 1 + random.below(5);
        for number in 0..count {
            let repeat = number > 0 && random.below(3) == 0;
            if !repeat {
                codes = OneCodePerField {
                    literal: random.below(20),
                    offset: random.below(8),
                    matched: random.below(36),
                };
            }
            let extras: Vec<(u64, u64, u64)> = (0..1 + random.below(6))
                .map(|_| {
                    (
                        random.next() & ((1 << LITERAL_LENGTHS.extras[codes.literal]) - 1),
                        random.next() & ((1 << OFFSETS.extras[codes.offset]) - 1),
                        random.next() & ((1 << MATCH_LENGTHS.extras[codes.matched]) - 1),
                    )
                })
                .collect();
            let needed: usize = extras
                .iter()
                .map(|extra| LITERAL_LENGTHS.bases[codes.literal] as usize + extra.0 as usize)
                .sum();
            let literals: Vec<u8> = (0..needed + random.below(4))
                .map(|_| random.next() as u8)
                .collect();
            valid = valid && model.block(&literals, codes, &extras).is_some();
            blocks.push(compressed(
                number + 1 == count,
                &raw_literals(3, &literals),
                &one_symbol_sequences(codes, repeat, &extras),
            ));
        }
        let frame = windowed(17, &blocks);
        let what = format!("round {round}");
        expect(&frame, valid.then_some(&model.output[..]), &what);
        if round % 40 == 0 {
            every_flip(&frame, model.output.len() + 4096, &what);
        }
    }
}

#[test]
fn matches_reach_across_the_end_of_the_window_buffer_as_the_byte_at_a_time_model() {
    let mut random = Random(0x1357_9BDF_0246_8ACE);
    for round in 0..300 {
        let mut model = ByteAtATimeModel::new();
        let mut blocks = Vec::new();
        let total = 30 + random.below(20);
        for number in 0..total {
            let before = model.output.len();
            let codes = OneCodePerField {
                literal: random.below(16),
                offset: if before < 600 {
                    2 + random.below(5)
                } else {
                    6 + random.below(4)
                },
                matched: random.below(34),
            };
            let mut extras = Vec::new();
            let mut literals = Vec::new();
            let mut size = 0;
            loop {
                let extra = (
                    0u64,
                    random.next() & ((1 << OFFSETS.extras[codes.offset]) - 1),
                    random.next() & ((1 << MATCH_LENGTHS.extras[codes.matched]) - 1),
                );
                let step =
                    codes.literal + MATCH_LENGTHS.bases[codes.matched] as usize + extra.2 as usize;
                let offset = (1usize << codes.offset) + extra.1 as usize - 3;
                if size + step > 1000 || offset > before + size + codes.literal || offset > 1024 {
                    break;
                }
                size += step;
                literals.extend((0..codes.literal).map(|_| random.next() as u8));
                extras.push(extra);
            }
            literals.extend((0..random.below(20)).map(|_| random.next() as u8));
            if extras.is_empty() {
                model.output.extend(&literals);
                blocks.push(compressed(
                    number + 1 == total,
                    &raw_literals(3, &literals),
                    &[0],
                ));
                continue;
            }
            model.block(&literals, codes, &extras).unwrap();
            blocks.push(compressed(
                number + 1 == total,
                &raw_literals(3, &literals),
                &one_symbol_sequences(codes, false, &extras),
            ));
        }
        let frame = windowed(10, &blocks);
        assert!(
            libzstd(&frame).as_deref() == Ok(&model.output[..]),
            "round {round}: libzstd differs from the model"
        );
        expect(&frame, Some(&model.output[..]), &format!("round {round}"));
    }
}

#[test]
fn a_streamed_match_from_beyond_the_window_is_refused() {
    let mut random = Random(0x0BAD_C0DE_0BAD_C0DE);
    let (mut blocks, mut bytes) = history(&mut random, 2000, 1000);
    let codes = OneCodePerField {
        literal: 0,
        offset: 10,
        matched: 0,
    };
    blocks.push(compressed(
        true,
        &raw_literals(0, b"x"),
        &one_symbol_sequences(codes, false, &[(0, 4, 0)]),
    ));
    let frame = windowed(10, &blocks);
    let error = ours(&frame).unwrap_err();
    assert!(error.contains(BEYOND_WINDOW), "{error}");
    let at_the_edge = one_symbol_sequences(codes, false, &[(0, 3, 0)]);
    blocks.pop();
    blocks.push(compressed(true, &raw_literals(0, b"x"), &at_the_edge));
    bytes.extend([bytes[976], bytes[977], bytes[978], b'x']);
    expect(
        &windowed(10, &blocks),
        Some(&bytes[..]),
        "a match of exactly the window",
    );
}

fn header(descriptor: u8, fields: &[u8], blocks: &[u8]) -> Vec<u8> {
    let mut frame = MAGIC.to_le_bytes().to_vec();
    frame.push(descriptor);
    frame.extend_from_slice(fields);
    frame.extend_from_slice(blocks);
    frame
}

#[test]
fn frame_headers_of_every_shape_read_as_libzstd_reads_them() {
    let content = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let body = block(0, true, content.len(), content);
    let size = content.len() as u64;
    for descriptor in 0..=255u8 {
        let single = descriptor & 0x20 != 0;
        for window in [0u8, 0x08, 0x57, 0x88, 0x8F, 0xA8, 0xFF] {
            for dictionary in [0u32, 1, 0x0101_0101] {
                for declared in [size, size + 1, 0, 255 + size, u64::MAX] {
                    let mut fields = Vec::new();
                    if !single {
                        fields.push(window);
                    }
                    let id = [0, 1, 2, 4][usize::from(descriptor & 3)];
                    fields.extend(&dictionary.to_le_bytes()[..id]);
                    match descriptor >> 6 {
                        0 if single => fields.push(declared as u8),
                        0 => {}
                        1 => fields.extend((declared.wrapping_sub(256) as u16).to_le_bytes()),
                        2 => fields.extend((declared as u32).to_le_bytes()),
                        _ => fields.extend(declared.to_le_bytes()),
                    }
                    let mut frame = header(descriptor, &fields, &body);
                    if descriptor & 4 != 0 {
                        let mut hash = Xxh64::new();
                        hash.update(content);
                        frame.extend((hash.digest() as u32).to_le_bytes());
                    }
                    let what = format!(
                        "descriptor {descriptor:#x} window {window:#x} dictionary {dictionary:#x} size {declared}"
                    );
                    same_outcome(&frame, 64, &|| what.clone());
                    if descriptor & 4 != 0 {
                        let end = frame.len() - 1;
                        frame[end] ^= 1;
                        same_outcome(&frame, 64, &|| format!("{what}, wrong checksum"));
                    }
                }
            }
        }
    }
}

#[test]
fn blocks_of_every_type_and_size_read_as_libzstd_reads_them() {
    let mut random = Random(0x1111_2222_3333_4444);
    let noise: Vec<u8> = (0..140_000).map(|_| random.next() as u8).collect();
    let empty = compressed(false, &raw_literals(0, b""), &[0]);
    for window_log in [10u32, 12, 17, 18] {
        for size in [0usize, 1, 1023, 1024, 1025, 4096, 4097, 131_072, 131_073] {
            for kind in 0..4u8 {
                let payload = match kind {
                    0 => &noise[..size],
                    1 => &noise[..1],
                    _ => &noise[..size.min(40)],
                };
                for last in [false, true] {
                    let mut blocks = vec![block(kind, last, size, payload)];
                    if !last {
                        blocks.push(empty.clone());
                        blocks.push(block(0, true, 3, b"end"));
                    }
                    let frame = windowed(window_log, &blocks);
                    same_outcome(&frame, size + 64, &|| {
                        format!("window {window_log}, type {kind}, size {size}, last {last}")
                    });
                }
            }
        }
    }
}

fn skippable(magic: u8, content: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x50 | magic, 0x2A, 0x4D, 0x18];
    frame.extend((content.len() as u32).to_le_bytes());
    frame.extend_from_slice(content);
    frame
}

#[test]
fn frames_in_a_row_and_skippable_frames_read_as_libzstd_reads_them() {
    let text = sources();
    let first = encode(&text[..5000], 3, |_| {});
    let second = encode(&text[5000..9000], 19, |encoder| {
        encoder.include_checksum(true).unwrap();
    });
    let pieces: Vec<Vec<u8>> = vec![
        first.clone(),
        second.clone(),
        skippable(0, b""),
        skippable(15, b"skipped"),
        skippable(7, &text[..300]),
        b"\x28\xB5\x2F".to_vec(),
        b"junk".to_vec(),
        vec![0x50, 0x2A, 0x4D, 0x18, 9, 0, 0, 0, 1],
        vec![0x50, 0x2A, 0x4D, 0x18, 0xFF, 0xFF, 0xFF, 0xFF],
        Vec::new(),
    ];
    for a in 0..pieces.len() {
        for b in 0..pieces.len() {
            for c in 0..pieces.len() {
                let joined = [&pieces[a][..], &pieces[b][..], &pieces[c][..]].concat();
                same_outcome(&joined, 20_000, &|| format!("pieces {a}, {b}, {c}"));
            }
        }
    }
}

struct Trickle<'a> {
    bytes: &'a [u8],
    step: usize,
}

impl Read for Trickle<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.step.min(buffer.len()).min(self.bytes.len());
        buffer[..count].copy_from_slice(&self.bytes[..count]);
        self.bytes = &self.bytes[count..];
        Ok(count)
    }
}

#[test]
fn any_input_and_output_buffer_sizes_give_the_same_bytes() {
    let mut random = Random(0x7777_8888_9999_AAAA);
    let text = sources();
    let input = mixed(&mut random, &text, 400_000);
    let mut frames = encode(&input, 5, |encoder| {
        encoder.include_checksum(true).unwrap();
        encoder.window_log(17).unwrap();
    });
    frames.extend(skippable(3, b"between"));
    frames.extend(encode(&input[..70_000], 1, |_| {}));
    let expected = [&input[..], &input[..70_000]].concat();
    for (step, capacity, chunk) in [
        (1, 1, 1),
        (1, 3, 7),
        (7, 2, 4096),
        (4096, 4096, 1),
        (65_536, 8192, 300_000),
    ] {
        let source = Trickle {
            bytes: &frames,
            step,
        };
        let mut reader = ZstdReader::new(std::io::BufReader::with_capacity(capacity, source));
        let mut decoded = Vec::new();
        let mut buffer = vec![0u8; chunk];
        loop {
            let read = reader.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            decoded.extend_from_slice(&buffer[..read]);
        }
        assert!(
            decoded == expected,
            "step {step}, capacity {capacity}, chunk {chunk}"
        );
    }
}

#[test]
fn a_single_frame_reader_stops_where_the_zstd_crate_stops() {
    let text = sources();
    let frame = encode(&text[..5000], 3, |encoder| {
        encoder.include_checksum(true).unwrap();
    });
    let tails: [&[u8]; 5] = [
        b"",
        b"tail",
        &frame,
        &skippable(1, b"abc"),
        b"\x28\xB5\x2F\xFD",
    ];
    let heads: [Vec<u8>; 3] = [frame.clone(), skippable(2, b"first"), skippable(0, b"")];
    for (number, head) in heads.iter().enumerate() {
        for tail in tails {
            let joined = [&head[..], tail].concat();
            let mut input = &joined[..];
            let mut decoded = Vec::new();
            let result = ZstdReader::new(&mut input)
                .single_frame()
                .read_to_end(&mut decoded)
                .map(|_| decoded);
            let mut theirs_input = &joined[..];
            let mut theirs = Vec::new();
            let theirs_result = zstd::stream::read::Decoder::with_buffer(&mut theirs_input)
                .unwrap()
                .single_frame()
                .read_to_end(&mut theirs)
                .map(|_| theirs);
            assert_eq!(result.ok(), theirs_result.ok(), "head {number}");
            assert_eq!(
                input.len(),
                theirs_input.len(),
                "head {number}: bytes left unread"
            );
            assert_eq!(input.len(), tail.len(), "head {number}");
        }
    }
}

#[test]
fn windows_from_the_smallest_to_past_the_limit_decode_or_are_refused_as_libzstd_does() {
    let mut random = Random(0xDEAD_BEEF_F00D_FACE);
    let text = sources();
    let mut input = mixed(&mut random, &text, 3_000_000);
    let start = input[..200_000].to_vec();
    input.extend_from_slice(&start);
    for window_log in [10u32, 11, 15, 18, 21, 22, 24, 27] {
        for long in [false, true] {
            let frame = encode(&input, 2, |encoder| {
                encoder.window_log(window_log).unwrap();
                encoder.long_distance_matching(long).unwrap();
            });
            let decoded = ours(&frame);
            assert!(
                decoded.as_deref() == Ok(&input[..]),
                "window {window_log}, long {long}"
            );
        }
    }
    for window_log in [28u32, 30, 31] {
        let frame = encode(&input[..100_000], 2, |encoder| {
            encoder.window_log(window_log).unwrap();
        });
        let error = ours(&frame).unwrap_err();
        assert!(
            error.contains("more than the 128 MiB Zipper accepts"),
            "{error}"
        );
        assert!(
            libzstd(&frame).is_err(),
            "libzstd now reads a 2^{window_log} window"
        );
        assert_eq!(
            ours_one_shot(&frame, 100_000).as_deref(),
            Ok(&input[..100_000])
        );
        assert_eq!(
            libzstd_one_shot(&frame, 100_000).as_deref(),
            Ok(&input[..100_000])
        );
    }
    let single = header(
        0xE0,
        &((1u64 << 27) + 1).to_le_bytes(),
        &block(1, true, 5, b"z"),
    );
    same_outcome(&single, 64, &|| {
        "a single segment one byte over 128 MiB".to_owned()
    });
    let over = header(
        0xE0,
        &((1u64 << 27) + 2).to_le_bytes(),
        &block(1, true, 5, b"z"),
    );
    same_outcome(&over, 64, &|| {
        "a single segment two bytes over 128 MiB".to_owned()
    });
}

#[test]
fn large_frames_with_sampled_damage_decode_or_are_refused_as_libzstd_does() {
    let mut random = Random(0xC001_D00D_5EED_5EED);
    let text = sources();
    let mut far = mixed(&mut random, &text, 700_000);
    let start = far[..50_000].to_vec();
    far.extend_from_slice(&start);
    let skew = skewed(&mut random, 300_000);
    let frames = [
        (
            "text at level 3",
            encode(&text[..400_000], 3, |_| {}),
            400_000,
        ),
        (
            "text at level 19",
            encode(&text[..400_000], 19, |_| {}),
            400_000,
        ),
        (
            "a 256 KiB window",
            encode(&far, 3, |encoder| {
                encoder.window_log(18).unwrap();
            }),
            far.len(),
        ),
        ("skewed literals", encode(&skew, 1, |_| {}), skew.len()),
    ];
    for (name, frame, decoded) in frames {
        assert_eq!(ours(&frame).map(|bytes| bytes.len()), Ok(decoded), "{name}");
        let mut damaged = frame.clone();
        for round in 0..2500 {
            let bit = random.below(frame.len() * 8);
            damaged[bit / 8] ^= 1 << (bit % 8);
            same_outcome(&damaged, decoded + 4096, &|| {
                format!("{name} round {round} bit {bit}")
            });
            damaged[bit / 8] ^= 1 << (bit % 8);
        }
    }
}

#[test]
fn every_corpus_file_with_a_zstandard_frame_decodes_as_libzstd_decodes_it() {
    let mut found = 0;
    let mut pending = vec![std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data"
    ))];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            if !bytes.starts_with(&MAGIC.to_le_bytes())
                && !bytes.starts_with(&[0x50, 0x2A, 0x4D, 0x18])
            {
                continue;
            }
            if path.extension().is_some_and(|extension| extension == "lz4") {
                continue;
            }
            found += 1;
            let theirs = libzstd(&bytes);
            assert!(theirs.is_ok(), "libzstd refuses {}", path.display());
            assert!(ours(&bytes) == theirs, "{}", path.display());
            let length = theirs.unwrap().len();
            for cut in (0..bytes.len()).step_by(bytes.len() / 200 + 1) {
                same_outcome(&bytes[..cut], length, &|| {
                    format!("{} cut to {cut}", path.display())
                });
            }
        }
    }
    assert!(
        found >= 3,
        "only {found} corpus files hold Zstandard frames"
    );
}

fn tree_of(weights: &[u8]) -> Option<Weights> {
    let mut listed = [0u8; SYMBOLS];
    listed[..weights.len()].copy_from_slice(weights);
    weights_from_list(listed, weights.len(), 0).ok()
}

fn both_tables_spell_the_same_codes(tree: &Weights) {
    let mut single = vec![0u16; HUFFMAN_SLOTS];
    let mut double = vec![0u32; HUFFMAN_SLOTS];
    let log = build_single(&mut single, tree, HUFFMAN_FAST_LOG);
    assert_eq!(build_double(&mut double, tree, HUFFMAN_FAST_LOG), log);
    assert_eq!(log, tree.log.max(HUFFMAN_FAST_LOG));
    let slots = 1usize << log;
    for slot in 0..slots {
        let symbol = usize::from(single[slot] >> 8);
        let bits = u32::from(single[slot] & 0xFF);
        assert!(symbol < tree.symbols && tree.weights[symbol] != 0);
        assert_eq!(bits, tree.log + 1 - u32::from(tree.weights[symbol]));
        assert_eq!(single[slot], single[slot >> (log - bits) << (log - bits)]);
        let after = single[(slot << bits) & (slots - 1)];
        let both = bits + u32::from(after & 0xFF);
        let expected = if both <= log {
            double_entry((after >> 8) as u8, both, symbol as u8, 2)
        } else {
            double_entry(symbol as u8, bits, 0, 1)
        };
        assert_eq!(
            double[slot],
            expected,
            "slot {slot} of {:?}",
            &tree.weights[..tree.symbols]
        );
    }
}

#[test]
fn both_huffman_tables_spell_the_same_codes_for_every_small_tree_and_random_deep_ones() {
    let mut checked = 0;
    let mut weights = [0u8; 6];
    'lists: loop {
        for listed in 1..=weights.len() {
            if let Some(tree) = tree_of(&weights[..listed]) {
                both_tables_spell_the_same_codes(&tree);
                checked += 1;
            }
        }
        for weight in weights.iter_mut() {
            *weight += 1;
            if *weight <= 6 {
                continue 'lists;
            }
            *weight = 0;
        }
        break;
    }
    assert!(checked > 3000, "only {checked} small trees are valid");
    let mut random = Random(0x7EE5_7EE5_7EE5_7EE5);
    for _ in 0..3000 {
        let mut depths = vec![1u8, 1];
        while depths.len() < 255 && random.below(40) != 0 {
            let at = random.below(depths.len());
            if u32::from(depths[at]) == HUFFMAN_LOG_MAX {
                continue;
            }
            depths[at] += 1;
            depths.insert(at, depths[at]);
        }
        let deepest = *depths.iter().max().unwrap();
        let leaves: Vec<u8> = depths.iter().map(|depth| deepest + 1 - depth).collect();
        let mut order: Vec<u8> = leaves;
        for index in (1..order.len()).rev() {
            order.swap(index, random.below(index + 1));
        }
        let last = order.pop().unwrap();
        for gap in 0..random.below(4) {
            if order.len() < 250 {
                order.insert(random.below(order.len() + 1), 0);
            }
            let _ = gap;
        }
        let tree = tree_of(&order).expect("a split tree is a valid tree");
        assert_eq!(tree.weights[order.len()], last);
        both_tables_spell_the_same_codes(&tree);
    }
}

#[test]
fn the_refill_schedule_keeps_every_field_of_a_sequence_inside_the_container() {
    let most = |code: &Code| u32::from(*code.extras.iter().max().unwrap());
    let state_bits = LITERAL_LENGTHS.max_log + MATCH_LENGTHS.max_log + OFFSETS.max_log;
    let after_a_refill = 7;
    for offset in 0..=most(&OFFSETS) {
        for matched in 0..=most(&MATCH_LENGTHS) {
            for literal in 0..=most(&LITERAL_LENGTHS) {
                let mut consumed = after_a_refill + offset + matched;
                assert!(consumed < 64);
                if offset + matched + literal >= REFILL_BEFORE_LITERAL_LENGTH {
                    consumed = after_a_refill;
                }
                assert!(consumed + literal + state_bits <= 64);
            }
        }
    }
    let first_fields = 8 + LITERAL_LENGTHS.max_log + OFFSETS.max_log + MATCH_LENGTHS.max_log;
    assert!(first_fields - 1 < 64);
}

#[test]
fn a_streamed_match_from_beyond_the_window_inside_one_buffer_lap_is_refused() {
    let mut random = Random(0x0BAD_5EED_0BAD_5EED);
    let (mut blocks, bytes) = history(&mut random, 1000, 1000);
    let literals: Vec<u8> = (0..100).map(|_| random.next() as u8).collect();
    let codes = OneCodePerField {
        literal: 25,
        offset: 10,
        matched: 0,
    };
    for (extra, refused) in [(3, false), (4, true), (60, true)] {
        blocks.truncate(1);
        blocks.push(compressed(
            true,
            &raw_literals(1, &literals),
            &one_symbol_sequences(codes, false, &[(36, extra, 0)]),
        ));
        let frame = windowed(10, &blocks);
        let from = 1100 - (1021 + extra as usize);
        let whole = [
            &bytes[..],
            &literals[..],
            &[&bytes[..], &literals[..]].concat()[from..from + 3],
        ]
        .concat();
        if refused {
            let error = ours(&frame).unwrap_err();
            assert!(error.contains(BEYOND_WINDOW), "{error}");
            assert_eq!(ours_one_shot(&frame, 4096).as_deref(), Ok(&whole[..]));
            assert_eq!(libzstd_one_shot(&frame, 4096).as_deref(), Ok(&whole[..]));
        } else {
            expect(&frame, Some(&whole[..]), "a match of exactly the window");
        }
    }
}

#[test]
fn a_block_that_decodes_to_more_than_the_largest_block_is_refused_as_libzstd_refuses_it() {
    let mut random = Random(0x51_2E_0F_B1_0C_4B_10_C4);
    let literals: Vec<u8> = (0..40).map(|_| random.next() as u8).collect();
    let codes = OneCodePerField {
        literal: 23,
        offset: 4,
        matched: 46,
    };
    for matched in [0u64, 1, 2, 17, 18, 49, 50, 51, 200, 511] {
        for spare in [0usize, 5, 31, 32, 33, 64, 65, 104, 105, 106, 4096] {
            let sequences = one_symbol_sequences(codes, false, &[(0, 0, matched)]);
            let frame = windowed(
                10,
                &[compressed(true, &raw_literals(1, &literals), &sequences)],
            );
            let decoded = 40 + 1027 + matched as usize;
            let what = || format!("a block of {decoded} bytes into {} bytes", decoded + spare);
            let repeated = windowed(
                10,
                &[compressed(true, &repeated_literals(1, 7, 40), &sequences)],
            );
            for frame in [&frame, &repeated] {
                same_outcome(frame, decoded + spare, &what);
                assert!(ours_one_shot(frame, decoded + spare).is_err(), "{}", what());
            }
        }
    }
}

#[test]
fn an_oversized_block_whose_literals_stay_in_the_block_is_refused_with_room_to_spare() {
    let mut random = Random(0x0B_5E_55_ED_B1_0C_4B_11);
    let literals: Vec<u8> = (0..16 * 40).map(|_| random.next() as u8).collect();
    let codes = OneCodePerField {
        literal: 23,
        offset: 4,
        matched: 46,
    };
    let sequences = one_symbol_sequences(codes, false, &[(0, 0, 0); 16]);
    let block = compressed(true, &raw_literals(1, &literals), &sequences);
    assert!(
        sequences.len() >= WILDCOPY,
        "the literals must be referenced"
    );
    let frame = windowed(10, &[block]);
    let decoded = 16 * (40 + 1027);
    assert!(libzstd(&frame).is_err());
    for spare in [0, 4096, 2 * BLOCK_MAX] {
        let refusal = ours_one_shot(&frame, decoded + spare);
        assert!(
            refusal
                .as_ref()
                .is_err_and(|error| error.contains(PAST_SIZE)),
            "{spare} spare bytes: {:?}",
            refusal.map(|bytes| bytes.len())
        );
    }
}

fn bit_at_a_time(container: u64, consumed: u32, count: u32) -> usize {
    (0..count).fold(0, |value, bit| {
        value << 1 | (container >> (63 - consumed - bit)) as usize & 1
    })
}

#[test]
fn a_state_keeps_its_fields_and_finds_its_next_state_as_a_bit_at_a_time_reader_does() {
    let mut random = Random(0x5EED_0F5E_0DE5_7A7E);
    for log in 0..=LITERAL_LENGTHS.max_log {
        for position in 1u16..(2 << log) {
            let (next, bits) = state_step(position, log);
            for extra in [0u8, 1, 7, 16, 31] {
                let base = random.next() as u32;
                let state = SequenceState::new(base, next, extra, bits);
                assert_eq!(
                    (state.base(), state.next(), state.extra(), state.bits()),
                    (
                        base as usize,
                        usize::from(next),
                        u32::from(extra),
                        u32::from(bits)
                    )
                );
                assert_eq!(state.bits_read(), u32::from(extra + bits));
                for consumed in 0..=64 - u32::from(bits) {
                    for container in [0, u64::MAX, random.next(), random.next()] {
                        let after = consumed + u32::from(bits);
                        let expected =
                            usize::from(next) + bit_at_a_time(container, consumed, bits.into());
                        assert_eq!(state.next_state(container, 64 - after), expected);
                        assert_eq!(
                            state.next_state(container, 0u32.wrapping_sub(after)),
                            expected
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_field_of_up_to_31_bits_reads_as_a_bit_at_a_time_reader_reads_it() {
    let mut random = Random(0xB175_A7A7_1ACE_0001);
    for count in 0..=31 {
        for consumed in 0..=64 - count {
            for container in [0, u64::MAX, random.next(), random.next(), random.next()] {
                let expected = bit_at_a_time(container, consumed, count);
                let after = consumed + count;
                assert_eq!(bits_at(container, 64 - after, count), expected);
                assert_eq!(
                    bits_at(container, 0u32.wrapping_sub(after), count),
                    expected
                );
            }
        }
    }
}

#[test]
fn a_match_from_the_last_lap_that_would_copy_past_the_buffer_takes_the_careful_path() {
    let block = padded();
    let table = SequenceTable::<512>::new();
    let tables = Tables {
        literal_lengths: &table,
        offsets: &table,
        match_lengths: &table,
    };
    let literals = LiteralRun {
        bytes: &block,
        at: 0,
        end: 0,
    };
    let (length, to, offset, matched) = (1000, 10, 60, 40);
    let mut output = vec![0u8; length];
    let bounds = Bounds {
        limit: length,
        fast_end: length - WILDCOPY,
        earlier: length - 8,
        window: usize::MAX,
    };
    let mut run = SequenceRun {
        at: FRONT,
        consumed: 0,
        literal: SequenceState::new(0, 0, 0, 0),
        offset_code: SequenceState::new(offset as u32, 0, 2, 0),
        matched: SequenceState::new(matched as u32, 0, 0, 0),
        repeats: [1, 4, 8],
        to,
        literal_at: 0,
        left: 1,
        largest_offset: 0,
        rare: (0, 0, 0),
    };
    let stop = fast_sequences(&mut run, &block, &tables, &literals, &bounds, &mut output);
    assert!(matches!(stop, Stop::Rare));
    assert_eq!((run.rare, run.to, run.left), ((0, offset, matched), to, 0));
}

fn states_stay_inside_their_table(counts: &[i16; SYMBOLS], max_symbol: usize, log: u32) {
    let size = 1usize << log;
    let expected = |symbol: usize| match counts[symbol] {
        -1 => 1,
        count => count as usize,
    };
    let mut table = SequenceTable::<512>::new();
    table.build(&OFFSETS, counts, max_symbol, log);
    let mut held = [0usize; SYMBOLS];
    for state in &table.states[..size] {
        assert!(state.bits() <= log && state.next() + (1 << state.bits()) <= size);
        assert_eq!(state.next() & ((1 << state.bits()) - 1), 0);
        assert_eq!(state.base(), OFFSETS.bases[state.extra() as usize] as usize);
        held[state.extra() as usize] += 1;
    }
    for (symbol, held) in held.iter().enumerate().take(max_symbol + 1) {
        assert_eq!(*held, expected(symbol), "symbol {symbol} of {counts:?}");
    }
    if log > WEIGHT_LOG_MAX {
        return;
    }
    let weights = weight_states(counts, max_symbol, log);
    let mut held = [0usize; SYMBOLS];
    for (state, sequence) in weights[..size].iter().zip(&table.states) {
        assert_eq!(usize::from(state.next), sequence.next());
        assert_eq!(u32::from(state.bits), sequence.bits());
        held[usize::from(state.symbol)] += 1;
    }
    for (symbol, held) in held.iter().enumerate().take(max_symbol + 1) {
        assert_eq!(*held, expected(symbol));
    }
}

#[test]
fn every_small_count_list_and_random_large_ones_build_states_that_stay_inside_their_table() {
    const SMALL: usize = 5;
    let mut checked = 0;
    let mut counts = [0i16; SYMBOLS];
    fn step(counts: &mut [i16; SYMBOLS], symbol: usize, left: i32, checked: &mut usize) {
        if symbol == SMALL - 1 {
            for last in [left as i16, -1] {
                if last == -1 && left != 1 {
                    continue;
                }
                counts[symbol] = last;
                states_stay_inside_their_table(counts, SMALL - 1, 5);
                *checked += 1;
            }
            return;
        }
        for count in -1..=left {
            let used = if count == -1 { 1 } else { count };
            if used > left {
                continue;
            }
            counts[symbol] = count as i16;
            step(counts, symbol + 1, left - used, checked);
        }
    }
    step(&mut counts, 0, 32, &mut checked);
    assert!(checked > 50_000, "only {checked} small count lists");
    let mut random = Random(0xF5E_F5E_F5E_F5E);
    for _ in 0..20_000 {
        let log = 5 + random.below(4) as u32;
        let symbols = 1 + random.below(OFFSETS.max_symbol + 1);
        let mut counts = [0i16; SYMBOLS];
        let mut left = 1i32 << log;
        for (symbol, held) in counts.iter_mut().enumerate().take(symbols) {
            let count = if symbol + 1 == symbols {
                left
            } else if random.below(4) == 0 && left > 0 {
                -1
            } else {
                let share = 1 + random.below(4);
                random.below(left as usize / share + 1) as i32
            };
            *held = count as i16;
            left -= if count == -1 { 1 } else { count };
        }
        states_stay_inside_their_table(&counts, symbols - 1, log);
    }
}

fn a_description_fills_its_table(bytes: &[u8], max_symbol: usize) -> bool {
    let mut counts = [0i16; SYMBOLS];
    let Ok((last, log, used)) = read_ncount(&mut counts, max_symbol, bytes) else {
        return false;
    };
    assert!((5..=15).contains(&log) && used <= bytes.len() && last <= max_symbol);
    if log == 15 {
        return false;
    }
    let filled: i32 = counts[..=max_symbol]
        .iter()
        .map(|&count| {
            assert!(count >= -1, "{bytes:02x?}");
            if count == -1 { 1 } else { i32::from(count) }
        })
        .sum();
    assert_eq!(filled, 1 << log, "{bytes:02x?} for {max_symbol} symbols");
    true
}

#[test]
fn table_descriptions_of_every_two_bytes_and_many_random_ones_fill_their_table_or_are_refused() {
    let codes = [
        OFFSETS.max_symbol,
        LITERAL_LENGTHS.max_symbol,
        MATCH_LENGTHS.max_symbol,
        255,
    ];
    let mut accepted = 0;
    for pair in 0..=u16::MAX {
        for max_symbol in codes {
            accepted += usize::from(a_description_fills_its_table(
                &pair.to_le_bytes(),
                max_symbol,
            ));
        }
    }
    let mut random = Random(0x4E43_4F55_4E54_5321);
    for round in 0..3_000_000 {
        let mut bytes = [0u8; 24];
        let length = random.below(bytes.len() + 1);
        for byte in bytes.iter_mut() {
            *byte = match round % 4 {
                0 => random.next() as u8,
                1 => [0x00, 0xFF, 0x0F, 0xF0][random.below(4)],
                2 => (random.next() & random.next()) as u8,
                _ => (random.next() | random.next()) as u8,
            };
        }
        let max_symbol = codes[random.below(codes.len())];
        accepted += usize::from(a_description_fills_its_table(&bytes[..length], max_symbol));
    }
    assert!(
        accepted > 10_000,
        "only {accepted} descriptions were accepted"
    );
}

#[test]
fn a_frame_with_a_large_window_touches_only_the_buffer_its_content_needs() {
    let frame = windowed(27, &[block(0, true, 1, b"a")]);
    let mut reader = ZstdReader::new(&frame[..]);
    let mut decoded = Vec::new();
    reader.read_to_end(&mut decoded).unwrap();
    assert_eq!(decoded, b"a");
    assert!(
        reader.ring.len() <= 2 * RING_SLACK,
        "a one-byte frame filled {} bytes of its window buffer",
        reader.ring.len()
    );
    let mut random = Random(0x6A0_6A0_6A0_6A0);
    let (blocks, bytes) = history(&mut random, 3_000_000, 100_000);
    let mut frame = windowed(27, &blocks);
    frame.extend(block(0, true, 0, b""));
    let mut reader = ZstdReader::new(&frame[..]);
    let mut decoded = Vec::new();
    reader.read_to_end(&mut decoded).unwrap();
    assert!(decoded == bytes);
    assert!(
        reader.ring.len() <= bytes.len() + 2 * RING_SLACK,
        "3 MB of content filled {} bytes of its window buffer",
        reader.ring.len()
    );
}

#[test]
fn a_single_segment_frame_of_unknown_size_reads_as_libzstd_reads_it() {
    let unknown = [0xFFu8; 8];
    let empty = header(0xE0, &unknown, &block(0, true, 0, b""));
    same_outcome(&empty, 64, &|| "an empty single segment".to_owned());
    assert!(ours(&empty).is_err(), "a 2^64 window passed the limit");
    let literals = raw_literals(1, b"some literals to copy from, twice over");
    let codes = OneCodePerField {
        literal: 16,
        offset: 4,
        matched: 6,
    };
    let sequences = one_symbol_sequences(codes, false, &[(0, 2, 0)]);
    let packed = header(0xE0, &unknown, &compressed(true, &literals, &sequences));
    same_outcome(&packed, 4096, &|| "a compressed single segment".to_owned());
    assert_eq!(
        ours_one_shot(&packed, 4096).map(|bytes| bytes.len()),
        Ok(47)
    );
}

#[test]
fn a_frame_whose_size_lies_before_an_empty_last_block_is_refused() {
    for last in [block(0, true, 0, b""), block(2, true, 0, b"")] {
        let lying = header(0x80, &[7, 53_978u32.to_le_bytes()[0], 0xD2, 0, 0], &last);
        let error = ours(&lying).unwrap_err();
        assert!(error.contains(SIZE_DIFFERS), "{error}");
        assert!(ours_one_shot(&lying, 64).is_err());
        assert!(libzstd_one_shot(&lying, 64).is_err());
        same_outcome(&lying, 64, &|| "a size that lies".to_owned());
    }
    let honest = header(0x80, &[7, 0, 0, 0, 0], &block(0, true, 0, b""));
    expect(
        &honest,
        Some(b""),
        "a size of zero before an empty last block",
    );
}

#[test]
fn a_frame_older_than_zstandard_one_is_named_in_its_refusal() {
    for version in 0x22..=0x27u8 {
        let frame = [version, 0xB5, 0x2F, 0xFD, 0, 0, 0, 0, 0];
        let error = ours(&frame).unwrap_err();
        assert!(error.contains("older than Zstandard 1.0"), "{error}");
        let error = ours_one_shot(&frame, 64).unwrap_err();
        assert!(error.contains("older than Zstandard 1.0"), "{error}");
    }
}

#[test]
fn a_second_decoder_on_a_thread_reuses_the_buffers_of_the_first() {
    let text = sources();
    let frame = encode(&text[..50_000], 3, |_| {});
    let address = |reader: &mut ZstdReader<&[u8]>| {
        let mut decoded = Vec::new();
        reader.read_to_end(&mut decoded).unwrap();
        assert!(decoded == text[..50_000]);
        std::ptr::from_ref(reader.decoder.held().block.as_ref()) as usize
    };
    let first = address(&mut ZstdReader::new(&frame[..]));
    let second = address(&mut ZstdReader::new(&frame[..]));
    assert_eq!(first, second);
    let raw = windowed(17, &[block(0, true, 3, b"raw")]);
    let mut reader = ZstdReader::new(&raw[..]);
    let mut decoded = Vec::new();
    reader.read_to_end(&mut decoded).unwrap();
    assert!(
        reader.decoder.0.is_none(),
        "a frame of raw blocks took decoder buffers"
    );
}

#[test]
fn one_read_fills_the_callers_buffer_across_blocks_and_keeps_a_later_error() {
    let text = sources();
    let frame = encode(&text[..600_000], 3, |_| {});
    let mut buffer = vec![0u8; 1 << 20];
    let mut reader = ZstdReader::new(&frame[..]);
    assert_eq!(reader.read(&mut buffer).unwrap(), 600_000);
    assert!(buffer[..600_000] == text[..600_000]);
    assert_eq!(reader.read(&mut buffer).unwrap(), 0);
    let mut reader = ZstdReader::new(&frame[..]);
    let mut total = 0;
    for _ in 0..3 {
        let read = reader.read(&mut buffer[total..total + 200_000]).unwrap();
        assert_eq!(read, 200_000);
        total += read;
    }
    assert!(buffer[..600_000] == text[..600_000]);
    let cut = &frame[..frame.len() - 40];
    let mut reader = ZstdReader::new(cut);
    let before = reader.read(&mut buffer).unwrap();
    assert!(
        (BLOCK_MAX..600_000).contains(&before),
        "{before} bytes before the cut"
    );
    assert!(buffer[..before] == text[..before]);
    let error = reader.read(&mut buffer).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    assert!(reader.read(&mut buffer).is_err());
}
