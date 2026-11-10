use std::io::{self, BufRead, Read};

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

impl<M: BufRead, C: Read, J: Read, R: Read> Bcj2<M, C, J, R> {
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
        if byte(&mut self.decisions, "truncated BCJ2 range coder")? != 0 {
            return Err(data_error("BCJ2 range coder does not start at zero"));
        }
        for _ in 1..RANGE_CODER_HEADER {
            self.code = (self.code << 8)
                | u32::from(byte(&mut self.decisions, "truncated BCJ2 range coder")?);
        }
        if self.code >= self.range {
            return Err(data_error("BCJ2 range coder starts outside its range"));
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

    fn take_pending(&mut self, buffer: &mut [u8]) -> usize {
        let pending = &self.pending[self.pending_start..];
        let taken = pending.len().min(buffer.len());
        buffer[..taken].copy_from_slice(&pending[..taken]);
        self.pending_start += taken;
        taken
    }

    fn copy_main(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let left = usize::try_from(self.size - self.position).unwrap_or(usize::MAX);
        let available = self.main.fill_buf()?;
        if available.is_empty() {
            return Err(data_error("truncated BCJ2 main stream"));
        }
        let window = &available[..available.len().min(buffer.len()).min(left)];
        let mut preceding = self.previous;
        let mut plain = 0;
        while plain < window.len() && !is_jump(preceding, window[plain]) {
            preceding = window[plain];
            plain += 1;
        }
        let opcode = window.get(plain).copied();
        let copied = plain + usize::from(opcode.is_some());
        buffer[..copied].copy_from_slice(&window[..copied]);
        self.main.consume(copied);
        self.position += copied as u64;
        self.previous = buffer[copied - 1];
        if let Some(opcode) = opcode {
            self.branch(opcode, preceding)?;
        }
        Ok(copied)
    }

    fn branch(&mut self, opcode: u8, preceding: u8) -> io::Result<()> {
        let index = match opcode {
            0xE8 => usize::from(preceding),
            0xE9 => JUMP_MODEL,
            _ => BRANCH_MODEL,
        };
        if !self.decide(index)? {
            return Ok(());
        }
        if self.size - self.position < TARGET as u64 {
            return Err(data_error(
                "BCJ2 converts a branch past the end of its output",
            ));
        }
        let mut target = [0u8; TARGET];
        let stream: &mut dyn Read = if opcode == 0xE8 {
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
        Ok(())
    }
}

impl<M: BufRead, C: Read, J: Read, R: Read> Read for Bcj2<M, C, J, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !self.started {
            self.start()?;
        }
        let mut written = 0;
        while written < buffer.len() {
            if self.pending_start < TARGET {
                written += self.take_pending(&mut buffer[written..]);
            } else if self.position < self.size {
                written += self.copy_main(&mut buffer[written..])?;
            } else {
                break;
            }
        }
        if written == 0 && !buffer.is_empty() && self.code != 0 {
            return Err(data_error("BCJ2 range coder does not end at zero"));
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
        while index < data.len() {
            let current = data[index];
            main.push(current);
            position += 1;
            if !is_jump(previous, current) {
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

    fn decode(streams: &Streams, size: u64, chunk: usize) -> (Vec<u8>, [usize; 4]) {
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
        let (main, calls, jumps, decisions) = decoder.into_streams();
        (
            output,
            [main.len(), calls.len(), jumps.len(), decisions.len()],
        )
    }

    struct ByteAtATime<M, C, J, R> {
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

    impl<M: Read, C: Read, J: Read, R: Read> ByteAtATime<M, C, J, R> {
        fn new(main: M, calls: C, jumps: J, decisions: R, size: u64) -> Self {
            ByteAtATime {
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

        fn start(&mut self) -> io::Result<()> {
            if byte(&mut self.decisions, "truncated BCJ2 range coder")? != 0 {
                return Err(data_error("BCJ2 range coder does not start at zero"));
            }
            for _ in 1..RANGE_CODER_HEADER {
                self.code = (self.code << 8)
                    | u32::from(byte(&mut self.decisions, "truncated BCJ2 range coder")?);
            }
            if self.code >= self.range {
                return Err(data_error("BCJ2 range coder starts outside its range"));
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
                self.probabilities[index] =
                    probability + ((MODEL_TOTAL - probability) >> MOVE_BITS);
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
            if !is_jump(self.previous, current) {
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

    impl<M: Read, C: Read, J: Read, R: Read> Read for ByteAtATime<M, C, J, R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.started {
                self.start()?;
            }
            let mut written = 0;
            while written < buffer.len()
                && (self.position < self.size || self.pending_start < TARGET)
            {
                buffer[written] = self.next_byte()?;
                written += 1;
            }
            if written == 0 && !buffer.is_empty() && self.code != 0 {
                return Err(data_error("BCJ2 range coder does not end at zero"));
            }
            Ok(written)
        }
    }

    struct Pieces<'a> {
        data: &'a [u8],
        lengths: &'a [usize],
        piece: usize,
        left: usize,
    }

    impl<'a> Pieces<'a> {
        fn new(data: &'a [u8], lengths: &'a [usize]) -> Self {
            Pieces {
                data,
                lengths,
                piece: 0,
                left: lengths[0],
            }
        }
    }

    impl Read for Pieces<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let available = self.fill_buf()?;
            let read = available.len().min(buffer.len());
            buffer[..read].copy_from_slice(&available[..read]);
            self.consume(read);
            Ok(read)
        }
    }

    impl BufRead for Pieces<'_> {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            while self.left == 0 && !self.data.is_empty() {
                self.piece = (self.piece + 1) % self.lengths.len();
                self.left = self.lengths[self.piece].max(1);
            }
            Ok(&self.data[..self.left.min(self.data.len())])
        }

        fn consume(&mut self, used: usize) {
            self.data = &self.data[used..];
            self.left -= used;
        }
    }

    type Outcome = Result<Vec<u8>, String>;

    fn outcomes(reader: &mut impl Read, chunk: usize) -> Vec<Outcome> {
        let mut buffer = vec![0u8; chunk];
        let mut seen = Vec::new();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => return seen,
                Ok(read) => seen.push(Ok(buffer[..read].to_vec())),
                Err(error) => {
                    seen.push(Err(error.to_string()));
                    return seen;
                }
            }
        }
    }

    fn agrees_with_byte_at_a_time(streams: &Streams, size: u64, lengths: &[usize], chunk: usize) {
        let (main, calls, jumps, decisions) = streams;
        let mut reference =
            ByteAtATime::new(&main[..], &calls[..], &jumps[..], &decisions[..], size);
        let expected = outcomes(&mut reference, chunk);
        let mut decoder = Bcj2::new(
            Pieces::new(main, lengths),
            &calls[..],
            &jumps[..],
            &decisions[..],
            size,
        );
        let seen = outcomes(&mut decoder, chunk);
        assert_eq!(
            seen, expected,
            "main {main:02x?} calls {calls:02x?} jumps {jumps:02x?} decisions {decisions:02x?} size {size} pieces {lengths:?} chunk {chunk}"
        );
        let (left_main, left_calls, left_jumps, left_decisions) = decoder.into_streams();
        assert_eq!(
            [
                left_main.data.len(),
                left_calls.len(),
                left_jumps.len(),
                left_decisions.len()
            ],
            [
                reference.main.len(),
                reference.calls.len(),
                reference.jumps.len(),
                reference.decisions.len()
            ],
            "unused bytes, main {main:02x?} size {size} pieces {lengths:?} chunk {chunk}"
        );
    }

    struct Random(u64);

    impl Random {
        fn below(&mut self, limit: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % limit as u64) as usize
        }

        fn byte(&mut self) -> u8 {
            const SPECIAL: [u8; 6] = [0xE8, 0xE9, 0x0F, 0x80, 0x8F, 0x85];
            match self.below(3) {
                0 => SPECIAL[self.below(SPECIAL.len())],
                _ => self.below(256) as u8,
            }
        }

        fn bytes(&mut self, length: usize) -> Vec<u8> {
            (0..length).map(|_| self.byte()).collect()
        }

        fn bytes_below(&mut self, limit: usize) -> Vec<u8> {
            let length = self.below(limit);
            self.bytes(length)
        }
    }

    #[test]
    fn every_main_stream_split_decodes_as_the_byte_at_a_time_reference() {
        let mut random = Random(0x9E37_79B9_7F4A_7C15);
        let mut inputs: Vec<Vec<u8>> = vec![
            vec![],
            vec![0xE8],
            vec![0x41, 0xE8],
            vec![0x0F, 0x85],
            vec![0x0F, 0x85, 1, 2, 3, 4],
            vec![0x0F, 0x0F, 0x85, 0xE8, 0xE9, 1, 2, 3, 4, 0x0F],
            vec![0xE8; 10],
            vec![0xE8, 0x0F, 0x80, 0x0F, 0x8F, 0x0F, 0x80, 0x0F, 0x8F, 0xE9],
            vec![0x00, 0x00, 0x0F, 0xE8, 0x0F, 0xE9, 0x0F, 0x0F, 0x8F],
        ];
        for length in [1, 2, 5, 9, 17, 33, 70] {
            for _ in 0..6 {
                inputs.push(random.bytes(length));
            }
        }
        let mut checked = 0;
        for input in &inputs {
            for choice in 0..6u64 {
                let streams = encode(input, |candidate| match choice {
                    0 => false,
                    1 => true,
                    2 => candidate % 2 == 0,
                    3 => candidate % 3 != 1,
                    _ => (0x5DEE_CE66D_u64.wrapping_mul(candidate as u64 + choice) >> 17) & 1 == 1,
                });
                let size = input.len() as u64;
                for first in 0..=streams.0.len() {
                    for rest in [1, 2, 3, 7, 64] {
                        for chunk in [1, 3, 5, 64] {
                            agrees_with_byte_at_a_time(&streams, size, &[first, rest], chunk);
                            checked += 1;
                        }
                    }
                }
                agrees_with_byte_at_a_time(&streams, size + 1, &[2, 5], 7);
                agrees_with_byte_at_a_time(&streams, size.saturating_sub(1), &[3], 7);
            }
        }
        assert!(checked > 50_000, "{checked}");
    }

    #[test]
    fn random_streams_decode_or_fail_as_the_byte_at_a_time_reference() {
        let mut random = Random(0x2545_F491_4F6C_DD1D);
        for _ in 0..40_000 {
            let main = random.bytes_below(24);
            let calls = random.bytes_below(12);
            let jumps = random.bytes_below(12);
            let mut decisions = random.bytes_below(10);
            if !decisions.is_empty() && random.below(8) != 0 {
                decisions[0] = 0;
            }
            if decisions.len() > 1 && random.below(2) == 0 {
                decisions[1] = random.below(0x80) as u8;
            }
            let size = match random.below(4) {
                0 => main.len() as u64,
                1 => (main.len() + calls.len() + jumps.len()) as u64,
                2 => random.below(40) as u64,
                _ => u64::MAX - random.below(3) as u64,
            };
            let first = random.below(main.len() + 1);
            let rest = 1 + random.below(6);
            let chunk = 1 + random.below(12);
            let streams = (main, calls, jumps, decisions);
            agrees_with_byte_at_a_time(&streams, size, &[first, rest], chunk);
        }
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
                        assert_eq!(left, [0, 0, 0, 0], "{input:02x?} mask {mask:05b}");
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
    fn a_range_coder_must_start_and_end_at_zero() {
        let data = b"no branch opcodes";
        for (decisions, refused) in [
            ([0, 0, 0, 0, 0], None),
            ([1, 0, 0, 0, 0], Some("does not start at zero")),
            ([0, 0, 0, 0, 1], Some("does not end at zero")),
            ([0, 0xFF, 0xFF, 0xFF, 0xFF], Some("outside its range")),
        ] {
            let mut decoder = Bcj2::new(&data[..], &[][..], &[][..], &decisions[..], 17);
            let mut output = Vec::new();
            match (decoder.read_to_end(&mut output), refused) {
                (Ok(_), None) => assert_eq!(output, data),
                (Err(error), Some(words)) => assert!(error.to_string().contains(words), "{error}"),
                (result, _) => panic!("{decisions:02x?}: {result:?}"),
            }
        }
    }

    #[test]
    fn a_branch_opcode_that_ends_the_output_takes_a_bit() {
        let data = [0x41, 0xE8];
        let (main, calls, jumps, decisions) = encode(&data, |_| false);
        let mut decoder = Bcj2::new(&main[..], &calls[..], &jumps[..], &decisions[..], 2);
        let mut output = Vec::new();
        decoder.read_to_end(&mut output).unwrap();
        assert_eq!(output, data);
        let mut coder = RangeEncoder::new();
        let mut probability = MODEL_TOTAL >> 1;
        coder.encode(&mut probability, true);
        let converted = coder.finish();
        let mut decoder = Bcj2::new(&main[..], &calls[..], &jumps[..], &converted[..], 2);
        let error = decoder.read_to_end(&mut Vec::new()).unwrap_err();
        assert!(error.to_string().contains("past the end"), "{error}");
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

    struct TwoPieces<'a> {
        first: &'a [u8],
        rest: &'a [u8],
    }

    impl Read for TwoPieces<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let available = self.fill_buf()?;
            let read = available.len().min(buffer.len());
            buffer[..read].copy_from_slice(&available[..read]);
            self.consume(read);
            Ok(read)
        }
    }

    impl BufRead for TwoPieces<'_> {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            Ok(if self.first.is_empty() {
                self.rest
            } else {
                self.first
            })
        }

        fn consume(&mut self, used: usize) {
            if self.first.is_empty() {
                self.rest = &self.rest[used..];
            } else {
                self.first = &self.first[used..];
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(10)]
    fn decoding_any_short_streams_never_panics() {
        let main: [u8; 4] = kani::any();
        let cut: usize = kani::any();
        kani::assume(cut <= main.len());
        let calls: [u8; 4] = kani::any();
        let jumps: [u8; 4] = kani::any();
        let decisions: [u8; 6] = kani::any();
        let size: u64 = kani::any();
        kani::assume(size <= 8);
        let (first, rest) = main.split_at(cut);
        let main = TwoPieces { first, rest };
        let mut decoder = Bcj2::new(main, &calls[..], &jumps[..], &decisions[..], size);
        let mut buffer = [0u8; 8];
        let _ = decoder.read(&mut buffer);
    }
}
