use crate::{BadField, Field};

const SHORT_MATCH_AFTER_RUN: usize = 0x0801;
const FAR_MATCH: usize = 0x4000;
const FIRST_RUN_BIAS: u8 = 17;
const LONG_RUN: usize = 15;
const MIDDLE_LENGTH: usize = 31;
const FAR_LENGTH: usize = 7;

struct Stream<'i, 'o> {
    input: &'i [u8],
    at: usize,
    output: &'o mut [u8],
    written: usize,
}

impl Stream<'_, '_> {
    fn byte(&mut self) -> Field<u8> {
        let byte = *self
            .input
            .get(self.at)
            .ok_or(BadField("LZO stream ends inside an instruction"))?;
        self.at += 1;
        Ok(byte)
    }

    fn distance_bytes(&mut self) -> Field<(usize, u8)> {
        let low = self.byte()?;
        let high = self.byte()?;
        Ok(((usize::from(high) << 6) | usize::from(low >> 2), low & 3))
    }

    fn extended(&mut self, base: usize) -> Field<usize> {
        let mut length = base;
        loop {
            match self.byte()? {
                0 => {
                    length = length
                        .checked_add(255)
                        .ok_or(BadField("LZO length overflows"))?;
                }
                last => return Ok(length + usize::from(last)),
            }
        }
    }

    fn literals(&mut self, count: usize) -> Field<()> {
        let end = self
            .at
            .checked_add(count)
            .filter(|end| *end <= self.input.len())
            .ok_or(BadField("LZO literal run passes the end of its input"))?;
        let out_end = self
            .written
            .checked_add(count)
            .filter(|end| *end <= self.output.len())
            .ok_or(BadField("LZO stream decodes past its block"))?;
        self.output[self.written..out_end].copy_from_slice(&self.input[self.at..end]);
        self.at = end;
        self.written = out_end;
        Ok(())
    }

    fn copy_match(&mut self, distance: usize, length: usize) -> Field<()> {
        if distance == 0 || distance > self.written {
            return Err(BadField("LZO match reaches before the start of its block"));
        }
        let end = self
            .written
            .checked_add(length)
            .filter(|end| *end <= self.output.len())
            .ok_or(BadField("LZO stream decodes past its block"))?;
        for position in self.written..end {
            self.output[position] = self.output[position - distance];
        }
        self.written = end;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum After {
    Match,
    LongRun,
    ShortRun,
}

// SPEC(LZO1X format, Linux Documentation/staging/lzo.rst): an instruction's top bits pick a run
// or a match and its low bits up to 3 literals after it; a far match of distance 0x4000 ends it.
pub(crate) fn decode(input: &[u8], output: &mut [u8]) -> Field<usize> {
    let mut stream = Stream {
        input,
        at: 0,
        output,
        written: 0,
    };
    let mut after = After::Match;
    if let Some(&first) = input.first()
        && first > FIRST_RUN_BIAS
    {
        stream.at = 1;
        let count = usize::from(first - FIRST_RUN_BIAS);
        stream.literals(count)?;
        after = if count < 4 {
            After::ShortRun
        } else {
            After::LongRun
        };
    }
    loop {
        let instruction = stream.byte()?;
        let state = if instruction >= 64 {
            let next = stream.byte()?;
            let distance = 1 + usize::from((instruction >> 2) & 7) + (usize::from(next) << 3);
            stream.copy_match(distance, usize::from(instruction >> 5) + 1)?;
            instruction & 3
        } else if instruction >= 32 {
            let length = match usize::from(instruction & 31) {
                0 => stream.extended(MIDDLE_LENGTH)?,
                length => length,
            };
            let (distance, state) = stream.distance_bytes()?;
            stream.copy_match(distance + 1, length + 2)?;
            state
        } else if instruction >= 16 {
            let length = match usize::from(instruction & 7) {
                0 => stream.extended(FAR_LENGTH)?,
                length => length,
            };
            let (distance, state) = stream.distance_bytes()?;
            let distance = distance + (usize::from(instruction & 8) << 11);
            if distance == 0 {
                return if stream.at == input.len() {
                    Ok(stream.written)
                } else {
                    Err(BadField("LZO stream has data after its end marker"))
                };
            }
            stream.copy_match(distance + FAR_MATCH, length + 2)?;
            state
        } else {
            match after {
                After::Match => {
                    let count = match usize::from(instruction) {
                        0 => stream.extended(LONG_RUN)?,
                        count => count,
                    };
                    stream.literals(count + 3)?;
                    after = After::LongRun;
                    continue;
                }
                After::LongRun | After::ShortRun => {
                    let next = stream.byte()?;
                    let distance = usize::from(instruction >> 2) + (usize::from(next) << 2);
                    match after {
                        After::LongRun => stream.copy_match(distance + SHORT_MATCH_AFTER_RUN, 3)?,
                        _ => stream.copy_match(distance + 1, 2)?,
                    }
                    instruction & 3
                }
            }
        };
        if state == 0 {
            after = After::Match;
        } else {
            stream.literals(usize::from(state))?;
            after = After::ShortRun;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const FIXTURES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/squashfs-corpus/lzo"
    );
    const LZOP_MAGIC: [u8; 9] = [0x89, b'L', b'Z', b'O', 0, 0x0D, 0x0A, 0x1A, 0x0A];
    const ADLER32_DATA: u32 = 1;
    const ADLER32_PACKED: u32 = 2;
    const FILTER: u32 = 0x800;
    const CRC32_DATA: u32 = 0x100;
    const CRC32_PACKED: u32 = 0x200;

    fn be32(bytes: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    fn lzop_blocks(file: &[u8]) -> Vec<(Vec<u8>, usize, bool)> {
        assert_eq!(file[..9], LZOP_MAGIC);
        let version = u16::from_be_bytes([file[9], file[10]]);
        let mut at = 9 + 4;
        if version >= 0x0940 {
            at += 2;
        }
        at += 1;
        if version >= 0x0940 {
            at += 1;
        }
        let flags = be32(file, at);
        at += 4;
        if flags & FILTER != 0 {
            at += 4;
        }
        at += 4 + 4;
        if version >= 0x0940 {
            at += 4;
        }
        let name = usize::from(file[at]);
        at += 1 + name + 4;
        let mut blocks = Vec::new();
        loop {
            let size = be32(file, at) as usize;
            at += 4;
            if size == 0 {
                return blocks;
            }
            let packed = be32(file, at) as usize;
            at += 4;
            at += 4 * usize::from(flags & ADLER32_DATA != 0)
                + 4 * usize::from(flags & CRC32_DATA != 0);
            let compressed = packed < size;
            if compressed {
                at += 4 * usize::from(flags & ADLER32_PACKED != 0)
                    + 4 * usize::from(flags & CRC32_PACKED != 0);
            }
            blocks.push((file[at..at + packed].to_vec(), size, compressed));
            at += packed;
        }
    }

    fn sample(length: usize, seed: u32) -> Vec<u8> {
        let mut state = seed | 1;
        let mut words = Vec::new();
        for index in 0..32 {
            words.push(format!("word{index}{}", "x".repeat(index % 7)));
        }
        let mut bytes = Vec::with_capacity(length);
        while bytes.len() < length {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            match state % 5 {
                0 => bytes.push(state as u8),
                1 => bytes.extend(std::iter::repeat_n(b'a', (state >> 8) as usize % 300)),
                _ => bytes.extend_from_slice(words[(state >> 8) as usize % 32].as_bytes()),
            }
        }
        bytes.truncate(length);
        bytes
    }

    fn lzop_decodes(file: &[u8], original: &[u8], label: &str) {
        let mut decoded = Vec::new();
        for (packed, size, compressed) in lzop_blocks(file) {
            if !compressed {
                decoded.extend_from_slice(&packed);
                continue;
            }
            let mut output = vec![0u8; size];
            let written =
                decode(&packed, &mut output).unwrap_or_else(|error| panic!("{label}: {error}"));
            assert_eq!(written, size, "{label}");
            decoded.extend_from_slice(&output);
            for short in [0, size / 2, size.saturating_sub(1)] {
                if short < size {
                    assert!(decode(&packed, &mut output[..short]).is_err(), "{label}");
                }
            }
        }
        assert!(decoded == original, "{label} decodes to other bytes");
    }

    #[test]
    fn lzop_output_of_every_level_decodes_to_its_input() {
        let fixtures: Vec<_> = std::fs::read_dir(FIXTURES)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "lzo"))
            .collect();
        assert!(fixtures.len() >= 4, "{fixtures:?}");
        for path in fixtures {
            let name = path.file_name().unwrap().to_str().unwrap();
            let source = path.with_file_name(&name[..name.find('.').unwrap() + 4]);
            let original = std::fs::read(&source).unwrap();
            let file = std::fs::read(&path).unwrap();
            assert!(
                lzop_blocks(&file)
                    .iter()
                    .any(|(_, _, compressed)| *compressed),
                "{}",
                path.display()
            );
            lzop_decodes(&file, &original, &path.display().to_string());
        }
        let Ok(lzop) = std::process::Command::new("lzop").arg("--version").output() else {
            return;
        };
        if !lzop.status.success() {
            return;
        }
        let work = tempfile::tempdir().unwrap();
        for (seed, length) in [(1, 1), (2, 3), (3, 4000), (4, 70_000), (5, 300_000)] {
            let original = sample(length, seed);
            let source = work.path().join(format!("sample-{seed}"));
            std::fs::write(&source, &original).unwrap();
            for level in ["-1", "-3", "-7", "-9"] {
                let packed = std::process::Command::new("lzop")
                    .args([level, "-c"])
                    .arg(&source)
                    .output()
                    .unwrap();
                assert!(packed.status.success());
                lzop_decodes(&packed.stdout, &original, &format!("sample {seed} {level}"));
            }
        }
    }

    #[test]
    fn every_cut_and_flip_of_a_stream_is_refused_or_stays_inside_its_block() {
        let path = Path::new(FIXTURES).join("words.txt.9.lzo");
        let file = std::fs::read(path).unwrap();
        let (packed, size, _) = lzop_blocks(&file).swap_remove(0);
        let mut output = vec![0u8; size];
        for cut in 0..packed.len() {
            assert!(decode(&packed[..cut], &mut output).is_err(), "cut {cut}");
        }
        let mut longer = packed.clone();
        longer.push(0);
        assert!(decode(&longer, &mut output).is_err());
        for at in 0..packed.len().min(4096) {
            for bit in [1u8, 0x10, 0x80] {
                let mut flipped = packed.clone();
                flipped[at] ^= bit;
                if let Ok(written) = decode(&flipped, &mut output) {
                    assert!(written <= size);
                }
            }
        }
    }

    #[test]
    fn the_smallest_streams_decode_as_lzo_writes_them() {
        let mut output = [0u8; 8];
        assert_eq!(decode(&[0x11, 0, 0], &mut output).ok(), Some(0));
        assert_eq!(
            decode(&[0x12, b'a', 0x11, 0, 0], &mut output).ok(),
            Some(1),
            "one literal"
        );
        assert_eq!(output[0], b'a');
        assert!(decode(&[], &mut output).is_err());
        assert!(decode(&[0x11, 0], &mut output).is_err());
        assert!(decode(&[0x11, 0, 0, 0], &mut output).is_err());
        assert!(decode(&[0x12, b'a', 0x11, 0, 0], &mut output[..0]).is_err());
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(8)]
    fn decoding_any_short_stream_never_panics_or_passes_its_block() {
        let input: [u8; 6] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= input.len());
        let limit: usize = kani::any();
        kani::assume(limit <= 4);
        let mut output = [0u8; 4];
        if let Ok(written) = decode(&input[..length], &mut output[..limit]) {
            assert!(written <= limit);
        }
    }
}
