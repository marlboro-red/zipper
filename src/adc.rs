use crate::{BadField, Field, field_check};

const LITERAL_RUN: u8 = 0x80;
const LONG_MATCH: u8 = 0x40;
const LONG_MATCH_LENGTH: usize = 4;
const SHORT_MATCH_LENGTH: usize = 3;

struct Step {
    literals: usize,
    length: usize,
    distance: usize,
    read: usize,
}

// EXTERNAL(7-Zip DmgHandler CAdcDecoder): a run of 1 to 128 literals, a 2-byte match of 3 to 18
// bytes up to 1024 back, or a 3-byte match of 4 to 67 bytes up to 65536 back.
fn step(input: &[u8]) -> Field<Step> {
    let first = input[0];
    if first & LITERAL_RUN != 0 {
        return Ok(Step {
            literals: usize::from(first - LITERAL_RUN) + 1,
            length: 0,
            distance: 0,
            read: 1,
        });
    }
    let second = usize::from(
        *input
            .get(1)
            .ok_or(BadField("ADC chunk ends inside a match"))?,
    );
    if first & LONG_MATCH != 0 {
        let third = usize::from(
            *input
                .get(2)
                .ok_or(BadField("ADC chunk ends inside a match"))?,
        );
        return Ok(Step {
            literals: 0,
            length: usize::from(first - LONG_MATCH) + LONG_MATCH_LENGTH,
            distance: (second << 8 | third) + 1,
            read: 3,
        });
    }
    Ok(Step {
        literals: 0,
        length: usize::from(first >> 2) + SHORT_MATCH_LENGTH,
        distance: (usize::from(first & 3) << 8 | second) + 1,
        read: 2,
    })
}

pub(crate) fn decode(input: &[u8], output: &mut [u8]) -> Field<()> {
    let mut at = 0usize;
    let mut written = 0usize;
    while written < output.len() {
        field_check(at < input.len(), "ADC chunk ends before its declared size")?;
        let step = step(&input[at..])?;
        at += step.read;
        let room = output.len() - written;
        if step.literals != 0 {
            field_check(
                step.literals <= room,
                "ADC chunk decodes past its declared size",
            )?;
            let literals = input
                .get(at..at + step.literals)
                .ok_or(BadField("ADC chunk ends inside a literal run"))?;
            output[written..written + step.literals].copy_from_slice(literals);
            at += step.literals;
            written += step.literals;
        } else {
            field_check(
                step.length <= room,
                "ADC chunk decodes past its declared size",
            )?;
            field_check(
                step.distance <= written,
                "ADC match reaches back before the start of its output",
            )?;
            crate::lzvn::copy_match(output, written, step.distance, step.length);
            written += step.length;
        }
    }
    field_check(at == input.len(), "ADC chunk has data after its end")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(input: &[u8], size: usize) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut at = 0;
        while out.len() < size {
            let b = *input.get(at)? as usize;
            at += 1;
            if b >= 0x80 {
                for _ in 0..b - 0x7F {
                    if out.len() == size {
                        return None;
                    }
                    out.push(*input.get(at)?);
                    at += 1;
                }
                continue;
            }
            let b1 = *input.get(at)? as usize;
            at += 1;
            let (length, distance) = if b >= 0x40 {
                let b2 = *input.get(at)? as usize;
                at += 1;
                (b - 0x40 + 4, b1 * 256 + b2 + 1)
            } else {
                (b / 4 + 3, (b % 4) * 256 + b1 + 1)
            };
            if length > size - out.len() || distance > out.len() {
                return None;
            }
            for _ in 0..length {
                out.push(out[out.len() - distance]);
            }
        }
        (at == input.len()).then_some(out)
    }

    #[test]
    fn every_short_chunk_decodes_as_a_byte_at_a_time_reference_does() {
        let alphabet = [
            0x00u8, 0x01, 0x04, 0x3F, 0x40, 0x41, 0x7F, 0x80, 0x81, 0x83, 0xFF,
        ];
        let mut cases = 0;
        for length in 0..=6 {
            let mut digits = vec![0usize; length];
            'all: loop {
                let chunk: Vec<u8> = digits.iter().map(|&d| alphabet[d]).collect();
                for size in [0, 1, 3, 4, 5, 8, 300] {
                    let mut output = vec![0u8; size];
                    let ours = decode(&chunk, &mut output).ok();
                    let theirs = reference(&chunk, size);
                    assert_eq!(ours.is_some(), theirs.is_some(), "{chunk:02x?} {size}");
                    if let Some(theirs) = theirs {
                        assert_eq!(output, theirs);
                    }
                    cases += 1;
                }
                for digit in digits.iter_mut() {
                    *digit += 1;
                    if *digit < alphabet.len() {
                        continue 'all;
                    }
                    *digit = 0;
                }
                break;
            }
        }
        assert!(cases > 10_000_000, "{cases}");
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(9)]
    fn decoding_any_short_chunk_never_panics_or_writes_past_its_output() {
        let input: [u8; 8] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= input.len());
        let mut output: [u8; 6] = kani::any();
        let size: usize = kani::any();
        kani::assume(size <= output.len());
        let _ = decode(&input[..length], &mut output[..size]);
    }
}
