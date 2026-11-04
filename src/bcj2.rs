use std::io::{self, Read};

const TOP: u32 = 1 << 24;
const MODEL_BITS: u32 = 11;
const MODEL_TOTAL: u16 = 1 << MODEL_BITS;
const MOVE_BITS: u32 = 5;
const RANGE_CODER_HEADER: usize = 5;
const JUMP_MODEL: usize = 256;
const BRANCH_MODEL: usize = 257;
const MODELS: usize = 258;
const TARGET: usize = 4;

fn is_jump(previous: u8, byte: u8) -> bool {
    byte & 0xFE == 0xE8 || (previous == 0x0F && byte & 0xF0 == 0x80)
}

fn data_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn byte(stream: &mut impl Read, what: &'static str) -> io::Result<u8> {
    let mut one = [0u8; 1];
    match stream.read(&mut one)? {
        1 => Ok(one[0]),
        _ => Err(data_error(what)),
    }
}

pub(crate) struct Bcj2<M, C, J, R> {
    main: M,
    calls: C,
    jumps: J,
    decisions: R,
    probabilities: [u16; MODELS],
    range: u32,
    code: u32,
    previous: u8,
    position: u64,
    size: u64,
    pending: [u8; TARGET],
    pending_start: usize,
    started: bool,
}

impl<M: Read, C: Read, J: Read, R: Read> Bcj2<M, C, J, R> {
    pub(crate) fn new(main: M, calls: C, jumps: J, decisions: R, size: u64) -> Self {
        Bcj2 {
            main,
            calls,
            jumps,
            decisions,
            probabilities: [MODEL_TOTAL >> 1; MODELS],
            range: u32::MAX,
            code: 0,
            previous: 0,
            position: 0,
            size,
            pending: [0; TARGET],
            pending_start: TARGET,
            started: false,
        }
    }

    pub(crate) fn into_streams(self) -> (M, C, J, R) {
        (self.main, self.calls, self.jumps, self.decisions)
    }

    fn start(&mut self) -> io::Result<()> {
        for _ in 0..RANGE_CODER_HEADER {
            self.code = (self.code << 8)
                | u32::from(byte(&mut self.decisions, "truncated BCJ2 range coder")?);
        }
        self.started = true;
        Ok(())
    }

    fn decide(&mut self, index: usize) -> io::Result<bool> {
        let probability = self.probabilities[index];
        let bound = (self.range >> MODEL_BITS) * u32::from(probability);
        let converted = self.code >= bound;
        if converted {
            self.range -= bound;
            self.code -= bound;
            self.probabilities[index] = probability - (probability >> MOVE_BITS);
        } else {
            self.range = bound;
            self.probabilities[index] = probability + ((MODEL_TOTAL - probability) >> MOVE_BITS);
        }
        if self.range < TOP {
            self.range <<= 8;
            self.code = (self.code << 8)
                | u32::from(byte(&mut self.decisions, "truncated BCJ2 range coder")?);
        }
        Ok(converted)
    }

    fn next_byte(&mut self) -> io::Result<u8> {
        if self.pending_start < TARGET {
            let byte = self.pending[self.pending_start];
            self.pending_start += 1;
            return Ok(byte);
        }
        let current = byte(&mut self.main, "truncated BCJ2 main stream")?;
        self.position += 1;
        if !is_jump(self.previous, current) || self.position == self.size {
            self.previous = current;
            return Ok(current);
        }
        let index = match current {
            0xE8 => usize::from(self.previous),
            0xE9 => JUMP_MODEL,
            _ => BRANCH_MODEL,
        };
        if !self.decide(index)? {
            self.previous = current;
            return Ok(current);
        }
        if self.size - self.position < TARGET as u64 {
            return Err(data_error(
                "BCJ2 converts a branch past the end of its output",
            ));
        }
        let mut target = [0u8; TARGET];
        let stream: &mut dyn Read = if current == 0xE8 {
            &mut self.calls
        } else {
            &mut self.jumps
        };
        stream
            .read_exact(&mut target)
            .map_err(|_| data_error("truncated BCJ2 call or jump stream"))?;
        let relative =
            u32::from_be_bytes(target).wrapping_sub((self.position as u32).wrapping_add(4));
        self.pending = relative.to_le_bytes();
        self.pending_start = 0;
        self.position += TARGET as u64;
        self.previous = self.pending[TARGET - 1];
        Ok(current)
    }
}

impl<M: Read, C: Read, J: Read, R: Read> Read for Bcj2<M, C, J, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !self.started {
            self.start()?;
        }
        let mut written = 0;
        while written < buffer.len() && (self.position < self.size || self.pending_start < TARGET) {
            buffer[written] = self.next_byte()?;
            written += 1;
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RangeEncoder {
        low: u64,
        range: u32,
        cache: u8,
        cache_size: u64,
        out: Vec<u8>,
    }

    impl RangeEncoder {
        fn new() -> Self {
            RangeEncoder {
                low: 0,
                range: u32::MAX,
                cache: 0,
                cache_size: 1,
                out: Vec::new(),
            }
        }

        fn shift_low(&mut self) {
            if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
                let carry = (self.low >> 32) as u8;
                let mut next = self.cache;
                loop {
                    self.out.push(next.wrapping_add(carry));
                    next = 0xFF;
                    self.cache_size -= 1;
                    if self.cache_size == 0 {
                        break;
                    }
                }
                self.cache = (self.low >> 24) as u8;
            }
            self.cache_size += 1;
            self.low = (self.low & 0x00FF_FFFF) << 8;
        }

        fn encode(&mut self, probability: &mut u16, converted: bool) {
            let bound = (self.range >> MODEL_BITS) * u32::from(*probability);
            if converted {
                self.low += u64::from(bound);
                self.range -= bound;
                *probability -= *probability >> MOVE_BITS;
            } else {
                self.range = bound;
                *probability += (MODEL_TOTAL - *probability) >> MOVE_BITS;
            }
            while self.range < TOP {
                self.range <<= 8;
                self.shift_low();
            }
        }

        fn finish(mut self) -> Vec<u8> {
            for _ in 0..RANGE_CODER_HEADER {
                self.shift_low();
            }
            self.out
        }
    }

    type Streams = (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>);

    fn encode(data: &[u8], convert: impl Fn(usize) -> bool) -> Streams {
        let (mut main, mut calls, mut jumps) = (Vec::new(), Vec::new(), Vec::new());
        let mut coder = RangeEncoder::new();
        let mut probabilities = [MODEL_TOTAL >> 1; MODELS];
        let (mut previous, mut index, mut position, mut candidate) = (0u8, 0usize, 0u64, 0usize);
        let size = data.len() as u64;
        while index < data.len() {
            let current = data[index];
            main.push(current);
            position += 1;
            if !is_jump(previous, current) || position == size {
                previous = current;
                index += 1;
                continue;
            }
            let model = match current {
                0xE8 => usize::from(previous),
                0xE9 => JUMP_MODEL,
                _ => BRANCH_MODEL,
            };
            let converted = convert(candidate) && data.len() - index > TARGET;
            candidate += 1;
            coder.encode(&mut probabilities[model], converted);
            if !converted {
                previous = current;
                index += 1;
                continue;
            }
            let relative: [u8; TARGET] = data[index + 1..index + 1 + TARGET].try_into().unwrap();
            let target =
                u32::from_le_bytes(relative).wrapping_add((position as u32).wrapping_add(4));
            let stream = if current == 0xE8 {
                &mut calls
            } else {
                &mut jumps
            };
            stream.extend_from_slice(&target.to_be_bytes());
            previous = relative[TARGET - 1];
            position += TARGET as u64;
            index += 1 + TARGET;
        }
        (main, calls, jumps, coder.finish())
    }

    fn decode(streams: &Streams, size: u64, chunk: usize) -> (Vec<u8>, [usize; 3]) {
        let (main, calls, jumps, decisions) = streams;
        let mut decoder = Bcj2::new(&main[..], &calls[..], &jumps[..], &decisions[..], size);
        let mut output = Vec::new();
        let mut buffer = vec![0u8; chunk];
        loop {
            let read = decoder.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..read]);
        }
        let (main, calls, jumps, _) = decoder.into_streams();
        (output, [main.len(), calls.len(), jumps.len()])
    }

    #[test]
    fn every_short_input_round_trips_under_every_conversion_choice() {
        let alphabet = [0x00, 0xE8, 0xE9, 0x0F, 0x85, 0x41];
        let mut inputs = vec![Vec::new()];
        let mut checked = 0;
        for _ in 0..5 {
            inputs = inputs
                .iter()
                .flat_map(|input: &Vec<u8>| {
                    alphabet.iter().map(move |&byte| {
                        let mut next = input.clone();
                        next.push(byte);
                        next
                    })
                })
                .collect();
            for input in &inputs {
                for mask in 0u32..32 {
                    let streams = encode(input, |candidate| mask >> candidate & 1 == 1);
                    for chunk in [1, 3, 64] {
                        let (output, left) = decode(&streams, input.len() as u64, chunk);
                        assert_eq!(&output, input, "{input:02x?} mask {mask:05b} chunk {chunk}");
                        assert_eq!(left, [0, 0, 0], "{input:02x?} mask {mask:05b}");
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 200_000);
    }

    #[test]
    fn a_conversion_past_the_end_of_the_output_is_refused() {
        for size in 2..=TARGET as u64 {
            let mut coder = RangeEncoder::new();
            let mut probability = MODEL_TOTAL >> 1;
            coder.encode(&mut probability, true);
            let decisions = coder.finish();
            let main = [0xE8, 0, 0, 0];
            let calls = [0u8; TARGET];
            let mut decoder = Bcj2::new(&main[..], &calls[..], &[][..], &decisions[..], size);
            let error = decoder.read_to_end(&mut Vec::new()).unwrap_err();
            assert!(
                error.to_string().contains("past the end"),
                "size {size}: {error}"
            );
        }
    }

    #[test]
    fn short_streams_are_refused() {
        let data = [0x41, 0xE8, 1, 2, 3, 4, 0x41];
        let (main, calls, jumps, decisions) = encode(&data, |_| true);
        for cut in 0..calls.len() {
            let streams = (
                main.clone(),
                calls[..cut].to_vec(),
                jumps.clone(),
                decisions.clone(),
            );
            let (main, calls, jumps, decisions) = &streams;
            let mut decoder = Bcj2::new(&main[..], &calls[..], &jumps[..], &decisions[..], 7);
            assert!(
                decoder.read_to_end(&mut Vec::new()).is_err(),
                "calls cut to {cut}"
            );
        }
        for cut in 0..main.len() {
            let mut decoder = Bcj2::new(&main[..cut], &calls[..], &jumps[..], &decisions[..], 7);
            assert!(
                decoder.read_to_end(&mut Vec::new()).is_err(),
                "main cut to {cut}"
            );
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(10)]
    fn decoding_any_short_streams_never_panics() {
        let main: [u8; 4] = kani::any();
        let calls: [u8; 4] = kani::any();
        let jumps: [u8; 4] = kani::any();
        let decisions: [u8; 6] = kani::any();
        let size: u64 = kani::any();
        kani::assume(size <= 8);
        let mut decoder = Bcj2::new(&main[..], &calls[..], &jumps[..], &decisions[..], size);
        let mut buffer = [0u8; 8];
        let _ = decoder.read(&mut buffer);
    }
}
