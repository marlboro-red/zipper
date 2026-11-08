use crate::{BadField, Field, field_check};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tail {
    Exact,
    Ignored,
}

const END_PADDING: usize = 7;

struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Input<'_> {
    fn byte(&mut self) -> Field<u8> {
        let byte = *self
            .bytes
            .get(self.at)
            .ok_or(BadField("LZVN stream ends before its end marker"))?;
        self.at += 1;
        Ok(byte)
    }
}

struct Opcode {
    literals: usize,
    length: usize,
    distance: Option<usize>,
}

enum Step {
    Copy(Opcode),
    Nop,
    End,
}

// SPEC(Apple lzvn_decode_base.c): the opcode table, with the small, medium, large and previous
// distance forms, the literal and match runs, the no-ops at 0x0E and 0x16 and the end at 0x06.
fn opcode(first: u8, input: &mut Input) -> Field<Step> {
    let byte = usize::from(first);
    if first >= 0xE0 {
        let mut count = byte & 0xF;
        if count == 0 {
            count = usize::from(input.byte()?) + 16;
        }
        return Ok(Step::Copy(if first & 0x10 == 0 {
            Opcode {
                literals: count,
                length: 0,
                distance: None,
            }
        } else {
            Opcode {
                literals: 0,
                length: count,
                distance: None,
            }
        }));
    }
    field_check(
        first & 0xF0 != 0x70 && first & 0xF0 != 0xD0,
        "LZVN stream holds an undefined opcode",
    )?;
    if first & 0xE0 == 0xA0 {
        let second = usize::from(input.byte()?);
        let third = usize::from(input.byte()?);
        return Ok(Step::Copy(Opcode {
            literals: (byte >> 3) & 3,
            length: ((byte & 7) << 2 | second & 3) + 3,
            distance: Some(second >> 2 | third << 6),
        }));
    }
    let literals = byte >> 6;
    let length = ((byte >> 3) & 7) + 3;
    let distance = match byte & 7 {
        6 if literals == 0 => {
            return match length - 3 {
                0 => Ok(Step::End),
                1 | 2 => Ok(Step::Nop),
                _ => Err(BadField("LZVN stream holds an undefined opcode")),
            };
        }
        6 => None,
        7 => {
            let low = usize::from(input.byte()?);
            Some(usize::from(input.byte()?) << 8 | low)
        }
        high => Some(high << 8 | usize::from(input.byte()?)),
    };
    Ok(Step::Copy(Opcode {
        literals,
        length,
        distance,
    }))
}

pub(crate) fn decode(input: &[u8], output: &mut [u8], start: usize, tail: Tail) -> Field<usize> {
    field_check(start <= output.len(), "LZVN output starts past its end")?;
    let mut input = Input {
        bytes: input,
        at: 0,
    };
    let mut written = start;
    let mut distance = 0usize;
    loop {
        let first = input.byte()?;
        let step = match opcode(first, &mut input)? {
            Step::End => break,
            Step::Nop => continue,
            Step::Copy(step) => step,
        };
        if let Some(new) = step.distance {
            distance = new;
        }
        field_check(
            step.literals <= output.len() - written,
            "LZVN stream decodes past its declared size",
        )?;
        let literals = input
            .bytes
            .get(input.at..input.at + step.literals)
            .ok_or(BadField("LZVN stream ends inside its literals"))?;
        output[written..written + step.literals].copy_from_slice(literals);
        input.at += step.literals;
        written += step.literals;
        if step.length != 0 {
            field_check(
                distance != 0 && distance <= written,
                "LZVN match reaches back before the start of its output",
            )?;
            field_check(
                step.length <= output.len() - written,
                "LZVN stream decodes past its declared size",
            )?;
            copy_match(output, written, distance, step.length);
            written += step.length;
        }
    }
    field_check(
        written == output.len(),
        "LZVN stream ends before its declared size",
    )?;
    let padding = input
        .bytes
        .get(input.at..input.at + END_PADDING)
        .ok_or(BadField("LZVN stream ends inside its end padding"))?;
    field_check(
        padding.iter().all(|&byte| byte == 0),
        "LZVN end padding is not zero",
    )?;
    let end = input.at + END_PADDING;
    field_check(
        tail == Tail::Ignored || end == input.bytes.len(),
        "LZVN stream has data after its end",
    )?;
    Ok(end)
}

pub(crate) fn copy_match(output: &mut [u8], at: usize, distance: usize, length: usize) {
    if distance >= length {
        output.copy_within(at - distance..at - distance + length, at);
    } else {
        for index in at..at + length {
            output[index] = output[index - distance];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(input: &[u8], size: usize) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut at = 0;
        let mut distance = 0usize;
        let next = |at: &mut usize| -> Option<usize> {
            let byte = *input.get(*at)?;
            *at += 1;
            Some(usize::from(byte))
        };
        loop {
            let b = next(&mut at)?;
            let (literals, length, new_distance) = match b {
                0x06 => break,
                0x0E | 0x16 => continue,
                0x1E | 0x26 | 0x2E | 0x36 | 0x3E => return None,
                0x70..=0x7F | 0xD0..=0xDF => return None,
                0xE0 => (next(&mut at)? + 16, 0, None),
                0xE1..=0xEF => (b - 0xE0, 0, None),
                0xF0 => (0, next(&mut at)? + 16, None),
                0xF1..=0xFF => (0, b - 0xF0, None),
                0xA0..=0xBF => {
                    let s = next(&mut at)?;
                    let t = next(&mut at)?;
                    (
                        (b >> 3) & 3,
                        ((b & 7) << 2) + (s & 3) + 3,
                        Some((s >> 2) + (t << 6)),
                    )
                }
                _ if b & 7 == 6 => (b >> 6, ((b >> 3) & 7) + 3, None),
                _ if b & 7 == 7 => {
                    let low = next(&mut at)?;
                    let high = next(&mut at)?;
                    (b >> 6, ((b >> 3) & 7) + 3, Some(high * 256 + low))
                }
                _ => {
                    let low = next(&mut at)?;
                    (b >> 6, ((b >> 3) & 7) + 3, Some((b & 7) * 256 + low))
                }
            };
            if let Some(new) = new_distance {
                distance = new;
            }
            for _ in 0..literals {
                let byte = next(&mut at)?;
                if out.len() == size {
                    return None;
                }
                out.push(byte as u8);
            }
            for _ in 0..length {
                if distance == 0 || distance > out.len() || out.len() == size {
                    return None;
                }
                out.push(out[out.len() - distance]);
            }
        }
        (out.len() == size && input.get(at..at + 7)? == [0; 7] && at + 7 == input.len())
            .then_some(out)
    }

    #[test]
    fn every_short_stream_decodes_as_a_byte_at_a_time_reference_does() {
        let alphabet = [
            0x00u8, 0x01, 0x06, 0x0E, 0x1E, 0x47, 0x71, 0xA1, 0xE1, 0xE0, 0xF1, 0x0F,
        ];
        let mut cases = 0;
        for length in 0..=5 {
            let mut digits = vec![0usize; length];
            loop {
                let mut stream: Vec<u8> = digits.iter().map(|&d| alphabet[d]).collect();
                stream.push(0x06);
                stream.extend([0; 7]);
                for size in [0, 1, 2, 4, 8, 20] {
                    let mut output = vec![0u8; size];
                    let ours = decode(&stream, &mut output, 0, Tail::Exact).ok();
                    let theirs = reference(&stream, size);
                    assert_eq!(ours.is_some(), theirs.is_some(), "{stream:02x?} {size}");
                    if let Some(theirs) = theirs {
                        assert_eq!(output, theirs, "{stream:02x?}");
                    }
                    cases += 1;
                }
                let mut position = 0;
                loop {
                    if position == length {
                        break;
                    }
                    digits[position] += 1;
                    if digits[position] < alphabet.len() {
                        break;
                    }
                    digits[position] = 0;
                    position += 1;
                }
                if position == length {
                    break;
                }
            }
        }
        assert!(cases > 1_000_000, "{cases}");
    }

    #[test]
    fn padding_and_trailing_bytes_follow_the_tail_rule() {
        let stream = [0xE1, b'a', 0x06, 0, 0, 0, 0, 0, 0, 0, 9];
        let mut output = [0u8; 1];
        assert!(decode(&stream, &mut output, 0, Tail::Exact).is_err());
        assert_eq!(decode(&stream, &mut output, 0, Tail::Ignored).unwrap(), 10);
        assert_eq!(&output, b"a");
        assert!(decode(&stream[..9], &mut output, 0, Tail::Ignored).is_err());
        let mut bad = stream;
        bad[5] = 1;
        assert!(decode(&bad, &mut output, 0, Tail::Ignored).is_err());
    }

    #[test]
    fn matches_reach_into_the_output_of_earlier_blocks() {
        let mut output = *b"abc\0\0\0";
        let stream = [0xF3, 0x06, 0, 0, 0, 0, 0, 0, 0];
        assert!(decode(&stream, &mut output, 3, Tail::Exact).is_err());
        let stream = [0x00, 0x03, 0x06, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(decode(&stream, &mut output, 3, Tail::Exact).unwrap(), 10);
        assert_eq!(&output, b"abcabc");
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(10)]
    fn decoding_any_short_stream_never_panics_or_writes_past_its_output() {
        let input: [u8; 8] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= input.len());
        let mut output: [u8; 3] = kani::any();
        let size: usize = kani::any();
        kani::assume(size <= output.len());
        let start: usize = kani::any();
        let tail = if kani::any() {
            Tail::Exact
        } else {
            Tail::Ignored
        };
        if let Ok(read) = decode(&input[..length], &mut output[..size], start, tail) {
            assert!(read <= length && read >= 8);
            assert!(tail == Tail::Ignored || read == length);
            assert!(start <= size);
        }
    }
}
