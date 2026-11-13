use crate::bzip2::{
    BYTES_PER_LEVEL, Block, END_BITS, MAGIC_BITS, Magic, MsbFirstCrc32, Refusal, Runs, Scratch,
    Walk, bits48, decode_block, magic_at, next_magic,
};
use std::collections::VecDeque;
use std::io::{BufRead, Read};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, sync_channel};

pub(crate) const OUTPUT_PER_EXTRA_STREAM: u64 = 4096;
pub(crate) const LONGEST_BLOCK: usize = 4 * 1024 * 1024;
const USUAL_BLOCK_INPUT: u64 = 1024 * 1024;
const READ_STEP: usize = 256 * 1024;
const MOST_WORKERS: usize = 16;
const DECODED_IN_PLACE_BELOW: usize = 8 * 1024;
const MOST_DECODERS_WALKING_FROM_BOTH_ENDS: usize = 4;
const WORK_PER_BLOCK_BYTE: u64 = 6;
const LARGEST_BLOCK_WORK: u64 = WORK_PER_BLOCK_BYTE * 9 * BYTES_PER_LEVEL as u64;
const PER_WORKER: u64 = 2 * USUAL_BLOCK_INPUT + LARGEST_BLOCK_WORK;
const FREED_WORK_KEPT_RESIDENT: u64 = LARGEST_BLOCK_WORK;
const SECOND_WALK: u64 = 4 * 9 * BYTES_PER_LEVEL as u64;
const BESIDE_THE_WORKERS: u64 = 3 * LONGEST_BLOCK as u64 + 2 * LARGEST_BLOCK_WORK + SECOND_WALK;

#[cfg(test)]
pub(crate) struct Bzip2Streams<R: BufRead> {
    decoder: Option<bzip2::bufread::BzDecoder<R>>,
    streams: u64,
    decoded: u64,
}

fn too_many_streams() -> std::io::Error {
    std::io::Error::other(format!(
        "bzip2 has more streams than {OUTPUT_PER_EXTRA_STREAM} decoded bytes each"
    ))
}

#[cfg(test)]
impl<R: BufRead> Read for Bzip2Streams<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        while let Some(decoder) = &mut self.decoder {
            let read = decoder.read(buffer)?;
            if read > 0 || buffer.is_empty() {
                self.decoded += read as u64;
                return Ok(read);
            }
            let mut input = self.decoder.take().expect("a decoder").into_inner();
            if input.fill_buf()?.is_empty() {
                return Ok(0);
            }
            if self.streams > self.decoded / OUTPUT_PER_EXTRA_STREAM {
                return Err(too_many_streams());
            }
            self.streams += 1;
            self.decoder = Some(bzip2::bufread::BzDecoder::new(input));
        }
        Ok(0)
    }
}

#[cfg(test)]
impl<R: BufRead> Bzip2Streams<R> {
    pub(crate) fn new(input: R) -> Self {
        Self {
            decoder: Some(bzip2::bufread::BzDecoder::new(input)),
            streams: 1,
            decoded: 0,
        }
    }
}

pub(crate) fn block_workers(threads: usize) -> usize {
    threads.saturating_sub(1).min(MOST_WORKERS)
}

pub(crate) fn memory(threads: usize) -> u64 {
    block_workers(threads) as u64 * (PER_WORKER + FREED_WORK_KEPT_RESIDENT) + BESIDE_THE_WORKERS
}

fn walk_in_place(decoders_at_once: usize) -> Walk {
    if decoders_at_once <= MOST_DECODERS_WALKING_FROM_BOTH_ENDS {
        Walk::FromBothEnds
    } else {
        Walk::Onward
    }
}

pub(crate) fn memory_in_place(decoders_at_once: usize) -> u64 {
    let largest_block = 9 * BYTES_PER_LEVEL as u64;
    let held_input = 2 * (LONGEST_BLOCK + READ_STEP) as u64;
    let block_and_emitted_text = 2 * largest_block;
    let walks = match walk_in_place(decoders_at_once) {
        Walk::Onward => 4 * largest_block,
        Walk::FromBothEnds => 4 * largest_block + SECOND_WALK,
    };
    held_input + block_and_emitted_text + walks
}

pub(crate) fn many_streams<'a, R: BufRead + Send + 'a>(
    input: R,
    threads: usize,
    decoders_at_once: usize,
) -> Box<dyn Read + Send + 'a> {
    Box::new(ParallelBzip2::new(
        input,
        block_workers(threads),
        AfterAStream::AnotherMayStart,
        decoders_at_once,
    ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterAStream {
    AnotherMayStart,
    NothingMayFollow,
    TheRestIsLeft,
}

enum Stage {
    Header,
    Blocks,
    Finished,
    Failed,
}

type Decoded = Result<Block, Refusal>;

enum Step {
    Block {
        start: u64,
        cut: u64,
        level: u8,
        charge: u64,
        decoded: Receiver<Decoded>,
        thread: Option<std::thread::JoinHandle<()>>,
    },
    StreamEnd {
        stored_crc: u32,
    },
}

struct Queued {
    first_byte: u64,
    step: Step,
}

impl Queued {
    fn join(&mut self) -> std::io::Result<()> {
        if let Step::Block { thread, .. } = &mut self.step
            && let Some(thread) = thread.take()
        {
            thread
                .join()
                .map_err(|_| std::io::Error::other("a bzip2 block decoder panicked"))?;
        }
        Ok(())
    }
}

struct Front {
    runs: Runs,
    crc: MsbFirstCrc32,
    stored_crc: u32,
}

pub(crate) struct ParallelBzip2<R: BufRead> {
    input: R,
    after_a_stream: AfterAStream,
    on_threads: bool,
    walk_in_place: Walk,
    scratch: Scratch,
    workers: usize,
    budget: u64,
    charged: u64,
    blocks_queued: usize,
    held: Vec<u8>,
    held_at: u64,
    input_ended: bool,
    cursor: u64,
    searched: u64,
    level: u8,
    stage: Stage,
    queue: VecDeque<Queued>,
    front: Option<Front>,
    combined: u32,
    streams: u64,
    decoded: u64,
}

fn damaged(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn cut_short() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "bzip2 stream ends before its end marker",
    )
}

fn refused(refusal: Refusal) -> std::io::Error {
    match refusal {
        Refusal::Truncated => cut_short(),
        Refusal::Damaged(message) => damaged(message),
    }
}

fn block_too_long() -> std::io::Error {
    std::io::Error::other(format!(
        "bzip2 block does not end within {LONGEST_BLOCK} compressed bytes, so it is damaged or longer than any writer makes"
    ))
}

const UNMARKED_END: &str = "bzip2 block is followed by neither a block nor the end of its stream";
const STREAM_CRC_MISMATCH: &str = "bzip2 stream CRC mismatch";

fn stream_level(header: &[u8]) -> Option<u8> {
    match header {
        [b'B', b'Z', b'h', level @ b'1'..=b'9'] => Some(level - b'0'),
        _ => None,
    }
}

impl<R: BufRead> ParallelBzip2<R> {
    pub(crate) fn new(
        input: R,
        workers: usize,
        after_a_stream: AfterAStream,
        decoders_at_once: usize,
    ) -> Self {
        ParallelBzip2 {
            input,
            after_a_stream,
            on_threads: workers > 0,
            scratch: Scratch::default(),
            walk_in_place: if workers == 0 {
                walk_in_place(decoders_at_once)
            } else {
                Walk::Onward
            },
            workers: workers.max(1),
            budget: workers.max(1) as u64 * PER_WORKER,
            charged: 0,
            blocks_queued: 0,
            held: Vec::new(),
            held_at: 0,
            input_ended: false,
            cursor: 0,
            searched: 0,
            level: 9,
            stage: Stage::Header,
            queue: VecDeque::new(),
            front: None,
            combined: 0,
            streams: 1,
            decoded: 0,
        }
    }

    fn fill(&mut self) -> std::io::Result<bool> {
        if self.input_ended {
            return Ok(false);
        }
        let available = self.input.fill_buf()?;
        if available.is_empty() {
            self.input_ended = true;
            return Ok(false);
        }
        let taken = available.len().min(READ_STEP);
        self.held.extend_from_slice(&available[..taken]);
        self.input.consume(taken);
        Ok(true)
    }

    fn held_bit(&self, bit: u64) -> u64 {
        bit - self.held_at * 8
    }

    fn held_end(&self) -> u64 {
        (self.held_at + self.held.len() as u64) * 8
    }

    fn holds(&mut self, bits: u64) -> std::io::Result<bool> {
        while self.held_end() < self.cursor + bits {
            if !self.fill()? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn held_bytes(&self, first_byte: u64, end_byte: u64) -> &[u8] {
        &self.held[(first_byte - self.held_at) as usize..(end_byte - self.held_at) as usize]
    }

    fn release_what_is_confirmed(&mut self) {
        let unconfirmed = self.queue.front().map(|queued| queued.first_byte);
        let used = (unconfirmed.unwrap_or(self.cursor / 8) - self.held_at) as usize;
        if used >= self.held.len() / 2 {
            self.held.drain(..used);
            self.held_at += used as u64;
        }
    }

    fn header(&mut self) -> std::io::Result<()> {
        if !self.holds(32)? {
            return Err(cut_short());
        }
        let first_byte = self.cursor / 8;
        let Some(level) = stream_level(self.held_bytes(first_byte, first_byte + 4)) else {
            return Err(damaged("bzip2 stream does not start with its magic"));
        };
        self.level = level;
        self.cursor += 32;
        self.searched = self.cursor;
        self.combined = 0;
        self.stage = Stage::Blocks;
        Ok(())
    }

    fn next_cut(&mut self) -> std::io::Result<Option<u64>> {
        loop {
            let from = self.searched.max(self.cursor + 1);
            let longest_cut = self.cursor + LONGEST_BLOCK as u64 * 8;
            if let Some((bit, _)) = next_magic(&self.held, self.held_bit(from)) {
                self.searched = bit + self.held_at * 8;
                return Ok(Some(self.searched).filter(|&cut| cut <= longest_cut));
            }
            self.searched = self.held_end().saturating_sub(MAGIC_BITS - 1).max(from);
            if self.searched > longest_cut || !self.fill()? {
                return Ok(None);
            }
        }
    }

    fn start_blocks(&mut self) -> std::io::Result<()> {
        while self.blocks_queued < self.workers && self.holds(MAGIC_BITS)? {
            let started = match magic_at(&self.held, self.held_bit(self.cursor)) {
                Some(Magic::Block) => self.start_block()?,
                Some(Magic::End) if self.after_a_stream == AfterAStream::AnotherMayStart => {
                    self.queue_the_end_before_another_stream()?
                }
                _ => false,
            };
            if !started {
                break;
            }
        }
        Ok(())
    }

    fn start_block(&mut self) -> std::io::Result<bool> {
        let Some(cut) = self.next_cut()? else {
            return Ok(false);
        };
        let first_byte = self.cursor / 8;
        let held = (first_byte - self.held_at) as usize..(cut.div_ceil(8) - self.held_at) as usize;
        let bytes = &self.held[held];
        let charge = 2 * bytes.len() as u64
            + WORK_PER_BLOCK_BYTE * u64::from(self.level) * BYTES_PER_LEVEL as u64;
        if self.blocks_queued > 0 && self.charged + charge > self.budget {
            return Ok(false);
        }
        let (start, limit, level) = (
            self.cursor - first_byte * 8,
            cut - first_byte * 8,
            self.level,
        );
        let (sender, decoded) = sync_channel(1);
        let thread = if !self.on_threads {
            let walk = self.walk_in_place;
            let _ = sender.send(decode_block(
                bytes,
                start,
                limit,
                level,
                walk,
                &mut self.scratch,
            ));
            None
        } else if bytes.len() < DECODED_IN_PLACE_BELOW {
            let _ = sender.send(decode_block(
                bytes,
                start,
                limit,
                level,
                Walk::Onward,
                &mut self.scratch,
            ));
            None
        } else {
            let (theirs, sent) = (Arc::new(bytes.to_vec()), sender.clone());
            let ours = Arc::clone(&theirs);
            let spawned = std::thread::Builder::new().spawn(move || {
                let decoded = decode_block(
                    &theirs,
                    start,
                    limit,
                    level,
                    Walk::Onward,
                    &mut Scratch::default(),
                );
                let _ = sent.send(decoded);
            });
            if spawned.is_err() {
                let _ = sender.send(decode_block(
                    &ours,
                    start,
                    limit,
                    level,
                    Walk::Onward,
                    &mut self.scratch,
                ));
            }
            spawned.ok()
        };
        self.queue.push_back(Queued {
            first_byte,
            step: Step::Block {
                start: self.cursor,
                cut,
                level,
                charge,
                decoded,
                thread,
            },
        });
        self.charged += charge;
        self.blocks_queued += 1;
        self.cursor = cut;
        Ok(true)
    }

    fn queue_the_end_before_another_stream(&mut self) -> std::io::Result<bool> {
        let header = (self.cursor + END_BITS).div_ceil(8) * 8;
        let first_block = header + 32;
        if !self.holds(first_block + MAGIC_BITS - self.cursor)? {
            return Ok(false);
        }
        let Some(level) = stream_level(self.held_bytes(header / 8, first_block / 8)) else {
            return Ok(false);
        };
        if magic_at(&self.held, self.held_bit(first_block)) != Some(Magic::Block) {
            return Ok(false);
        }
        let Some(stored_crc) = bits48(&self.held, self.held_bit(self.cursor) + 32) else {
            return Ok(false);
        };
        self.queue.push_back(Queued {
            first_byte: self.cursor / 8,
            step: Step::StreamEnd {
                stored_crc: stored_crc as u32,
            },
        });
        self.level = level;
        self.cursor = first_block;
        self.searched = first_block;
        Ok(true)
    }

    fn emit(&mut self, block: Block) {
        self.front = Some(Front {
            runs: Runs::new(block.text),
            crc: MsbFirstCrc32::new(),
            stored_crc: block.stored_crc,
        });
    }

    fn another_stream_may_start(&mut self) -> std::io::Result<()> {
        if self.streams > self.decoded / OUTPUT_PER_EXTRA_STREAM {
            return Err(too_many_streams());
        }
        self.streams += 1;
        self.combined = 0;
        Ok(())
    }

    fn advance(&mut self) -> std::io::Result<()> {
        self.start_blocks()?;
        let Some(mut queued) = self.queue.pop_front() else {
            return self.pass_what_no_block_follows();
        };
        queued.join()?;
        match queued.step {
            Step::StreamEnd { stored_crc } if stored_crc == self.combined => {
                self.release_what_is_confirmed();
                self.another_stream_may_start()
            }
            Step::StreamEnd { .. } => Err(damaged(STREAM_CRC_MISMATCH)),
            Step::Block {
                start,
                cut,
                level,
                charge,
                decoded,
                ..
            } => {
                self.charged -= charge;
                self.blocks_queued -= 1;
                let decoded = decoded
                    .recv()
                    .map_err(|_| std::io::Error::other("a bzip2 block decoder stopped"))?;
                match decoded {
                    Ok(block) if block.end == cut - queued.first_byte * 8 => {
                        self.emit(block);
                        self.release_what_is_confirmed();
                        if self.on_threads {
                            self.start_blocks()?;
                        }
                        Ok(())
                    }
                    Ok(_) => Err(damaged(UNMARKED_END)),
                    Err(Refusal::Damaged(message)) => Err(damaged(message)),
                    Err(Refusal::Truncated) => {
                        self.drop_what_was_started_past_the_false_cut()?;
                        (self.cursor, self.level) = (start, level);
                        self.decode_past_the_false_cut()
                    }
                }
            }
        }
    }

    fn pass_what_no_block_follows(&mut self) -> std::io::Result<()> {
        if !self.holds(MAGIC_BITS)? {
            return Err(cut_short());
        }
        match magic_at(&self.held, self.held_bit(self.cursor)) {
            Some(Magic::End) => self.end_stream(),
            Some(Magic::Block) if self.input_ended => Err(cut_short()),
            Some(Magic::Block) => Err(block_too_long()),
            None => Err(damaged(UNMARKED_END)),
        }
    }

    fn drop_what_was_started_past_the_false_cut(&mut self) -> std::io::Result<()> {
        for mut later in std::mem::take(&mut self.queue) {
            later.join()?;
        }
        (self.charged, self.blocks_queued) = (0, 0);
        Ok(())
    }

    fn decode_past_the_false_cut(&mut self) -> std::io::Result<()> {
        loop {
            let held = self.held.len();
            let start = self.held_bit(self.cursor);
            let longest = start + LONGEST_BLOCK as u64 * 8;
            let limit = longest.min(held as u64 * 8);
            let walk = self.walk_in_place;
            match decode_block(
                &self.held,
                start,
                limit,
                self.level,
                walk,
                &mut self.scratch,
            ) {
                Ok(block) => {
                    self.cursor = block.end + self.held_at * 8;
                    self.searched = self.cursor;
                    if !self.holds(MAGIC_BITS)? {
                        return Err(cut_short());
                    }
                    if magic_at(&self.held, self.held_bit(self.cursor)).is_none() {
                        return Err(damaged(UNMARKED_END));
                    }
                    self.release_what_is_confirmed();
                    self.emit(block);
                    return Ok(());
                }
                Err(Refusal::Damaged(message)) => return Err(damaged(message)),
                Err(Refusal::Truncated) if limit == longest => return Err(block_too_long()),
                Err(Refusal::Truncated) => {
                    let of_the_block = held - (start / 8) as usize;
                    let wanted = held + of_the_block.clamp(READ_STEP, LONGEST_BLOCK);
                    while self.held.len() < wanted && self.fill()? {}
                    if self.held.len() == held {
                        return Err(cut_short());
                    }
                }
            }
        }
    }

    fn end_stream(&mut self) -> std::io::Result<()> {
        if !self.holds(END_BITS)? {
            return Err(cut_short());
        }
        let stored = bits48(&self.held, self.held_bit(self.cursor) + 32).map(|bits| bits as u32);
        if stored != Some(self.combined) {
            return Err(damaged(STREAM_CRC_MISMATCH));
        }
        self.cursor = (self.cursor + END_BITS).div_ceil(8) * 8;
        self.release_what_is_confirmed();
        if self.held_end() == self.cursor && !self.fill()? {
            self.stage = Stage::Finished;
            return Ok(());
        }
        match self.after_a_stream {
            AfterAStream::AnotherMayStart => {
                self.stage = Stage::Header;
                self.another_stream_may_start()
            }
            AfterAStream::NothingMayFollow => Err(damaged("data follows the bzip2 stream")),
            AfterAStream::TheRestIsLeft => {
                self.stage = Stage::Finished;
                Ok(())
            }
        }
    }

    fn read_front(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if let Some(front) = &mut self.front {
                let read = front.runs.read(buffer).map_err(refused)?;
                if read > 0 {
                    front.crc.update(&buffer[..read]);
                    self.decoded += read as u64;
                    return Ok(read);
                }
            }
            if let Some(front) = self.front.take() {
                let crc = front.crc.value();
                if crc != front.stored_crc {
                    return Err(damaged("bzip2 block CRC mismatch"));
                }
                self.combined = self.combined.rotate_left(1) ^ crc;
            }
            match self.stage {
                Stage::Header => self.header()?,
                Stage::Blocks => self.advance()?,
                Stage::Finished => return Ok(0),
                Stage::Failed => {
                    return Err(std::io::Error::other("an earlier bzip2 block failed"));
                }
            }
        }
    }
}

impl<R: BufRead> Drop for ParallelBzip2<R> {
    fn drop(&mut self) {
        for queued in &mut self.queue {
            let _ = queued.join();
        }
    }
}

impl<R: BufRead> Read for ParallelBzip2<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let read = self.read_front(buffer);
        if read.is_err() {
            self.front = None;
            self.stage = Stage::Failed;
        }
        read
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bzip2::tests::{
        Noise, Spare, compress, magic_bits, sample, with_bits_after_block, with_spare_bits,
    };
    use crate::bzip2::{BLOCK_MAGIC, END_MAGIC};

    fn reader(
        stream: &[u8],
        workers: usize,
        input_step: usize,
    ) -> ParallelBzip2<std::io::BufReader<&[u8]>> {
        ParallelBzip2::new(
            std::io::BufReader::with_capacity(input_step, stream),
            workers,
            AfterAStream::AnotherMayStart,
            1 + workers % 8,
        )
    }

    fn parallel(
        stream: &[u8],
        workers: usize,
        input_step: usize,
        read_step: usize,
    ) -> std::io::Result<Vec<u8>> {
        let mut reader = reader(stream, workers, input_step);
        let (mut decoded, mut piece) = (Vec::new(), vec![0u8; read_step]);
        loop {
            match reader.read(&mut piece)? {
                0 => return Ok(decoded),
                read => decoded.extend_from_slice(&piece[..read]),
            }
        }
    }

    fn serial(stream: &[u8]) -> Option<Vec<u8>> {
        let mut decoded = Vec::new();
        Bzip2Streams::new(stream)
            .read_to_end(&mut decoded)
            .is_ok()
            .then_some(decoded)
    }

    fn four_blocks(kind: usize, noise: &mut Noise) -> (Vec<u8>, Vec<u8>) {
        let data = sample(kind, 350_000, noise);
        let stream = compress(&data, 1);
        (data, stream)
    }

    #[test]
    fn blocks_come_out_in_order_for_every_worker_count_input_step_and_read_size() {
        let mut noise = Noise(21);
        for kind in 0..6 {
            let (data, stream) = four_blocks(kind, &mut noise);
            for workers in [0, 1, 2, 3, 8] {
                for (input_step, read_step) in [(13, 1 << 17), (4096, 4096), (1 << 20, 333)] {
                    let decoded = parallel(&stream, workers, input_step, read_step);
                    assert!(
                        decoded.as_ref().ok() == Some(&data),
                        "{kind} {workers} {input_step} {read_step}"
                    );
                }
            }
        }
        for level in 1..=9 {
            let data = sample(1, 230_000 * level as usize, &mut noise);
            let decoded = parallel(&compress(&data, level), 3, 65536, 65536);
            assert!(decoded.ok() == Some(data), "level {level}");
        }
    }

    #[test]
    fn concatenated_streams_decode_or_are_refused_as_the_serial_reader_does() {
        let mut noise = Noise(22);
        let empty = compress(&[], 9);
        let small = compress(&sample(1, 5000, &mut noise), 9);
        let (_, large) = four_blocks(1, &mut noise);
        let joined = |parts: &[&[u8]]| parts.concat();
        let mut cases = vec![
            joined(&[&small, &small]),
            joined(&[&large, &empty, &small, &large]),
            joined(&[&empty, &small]),
            joined(&[&small, &empty, &empty]),
            joined(&[&small, &empty, &empty, &empty]),
            joined(&[&large, b"BZ"]),
            joined(&[&large, &[0]]),
            joined(&[&large, b"BZh0"]),
            Vec::new(),
            b"BZh".to_vec(),
        ];
        cases.push(
            std::iter::repeat_n(&large[..], 3)
                .flatten()
                .copied()
                .collect(),
        );
        cases.push(
            std::iter::once(&large[..])
                .chain(std::iter::repeat_n(&empty[..], 90))
                .flatten()
                .copied()
                .collect(),
        );
        for (case, stream) in cases.iter().enumerate() {
            for workers in [0, 1, 3] {
                assert_eq!(
                    parallel(stream, workers, 4096, 4096).ok(),
                    serial(stream),
                    "case {case} workers {workers}"
                );
            }
        }
        assert!(serial(&cases[1]).is_some());
        assert!(serial(&cases[11]).is_none());
    }

    #[test]
    fn a_magic_planted_inside_any_block_is_passed_over_by_decoding_on() {
        let mut noise = Noise(23);
        let (data, stream) = four_blocks(0, &mut noise);
        for magic in [BLOCK_MAGIC, END_MAGIC] {
            for block in 0..4 {
                let once = with_spare_bits(&stream, block, Spare::Selectors, &magic_bits(magic));
                let again = with_spare_bits(&once, 3 - block, Spare::Selectors, &magic_bits(magic));
                for planted in [&once, &again] {
                    assert_eq!(serial(planted).as_ref(), Some(&data));
                    for workers in [0, 1, 2, 4] {
                        for input_step in [29, 1 << 20] {
                            let decoded = parallel(planted, workers, input_step, 65536);
                            assert!(
                                decoded.as_ref().ok() == Some(&data),
                                "{magic:x} block {block} workers {workers}: {:?}",
                                decoded.err()
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn bits_between_a_block_and_the_next_magic_are_refused() {
        let mut noise = Noise(30);
        let (data, stream) = four_blocks(1, &mut noise);
        assert_eq!(serial(&stream).as_ref(), Some(&data));
        for block in 0..4 {
            for extra in [1, 8, 13, 4000] {
                let padded = with_bits_after_block(&stream, block, &vec![false; extra]);
                assert_eq!(serial(&padded), None);
                for workers in [0, 1, 3] {
                    let decoded = parallel(&padded, workers, 4096, 65536);
                    assert!(
                        decoded.is_err(),
                        "block {block} extra {extra} workers {workers}"
                    );
                }
            }
        }
    }

    #[test]
    fn one_stream_alone_refuses_or_leaves_what_follows_it_as_the_bzip2_crate_does() {
        let mut noise = Noise(31);
        let (data, stream) = four_blocks(1, &mut noise);
        let small = compress(&sample(1, 5000, &mut noise), 9);
        for (follows, tail) in [
            (false, &b""[..]),
            (true, b"\0"),
            (true, &small),
            (true, b"BZ"),
        ] {
            let whole = [&stream[..], tail].concat();
            let mut crate_input = &whole[..];
            let mut crate_decoded = Vec::new();
            bzip2::bufread::BzDecoder::new(&mut crate_input)
                .read_to_end(&mut crate_decoded)
                .unwrap();
            assert!(crate_decoded == data && crate_input.is_empty() != follows);
            for workers in [0, 2] {
                for (after, refused) in [
                    (AfterAStream::NothingMayFollow, follows),
                    (AfterAStream::TheRestIsLeft, false),
                ] {
                    let mut reader = ParallelBzip2::new(&whole[..], workers, after, 1);
                    let mut decoded = Vec::new();
                    let outcome = reader.read_to_end(&mut decoded);
                    assert_eq!(outcome.is_err(), refused, "{workers} {follows}");
                    assert!(refused || decoded == data, "{workers} {follows}");
                }
            }
        }
    }

    #[test]
    fn a_planted_stream_end_and_header_inside_a_block_start_no_stream() {
        let mut noise = Noise(28);
        let data = sample(0, 650_000, &mut noise);
        let stream = compress(&data, 2);
        let mut planted = magic_bits(END_MAGIC);
        planted.extend([false; 32]);
        planted.extend((0..32).map(|bit| u32::from_be_bytes(*b"BZh1") >> (31 - bit) & 1 == 1));
        planted.extend(magic_bits(BLOCK_MAGIC));
        for block in 0..4 {
            let stream = with_spare_bits(&stream, block, Spare::SelectorsFromAWholeByte, &planted);
            let whole_byte_ends = (0..stream.len())
                .filter(|&byte| magic_at(&stream, byte as u64 * 8) == Some(Magic::End))
                .count();
            assert_eq!(whole_byte_ends, 1, "the planted end, not the true one");
            assert_eq!(serial(&stream).as_ref(), Some(&data));
            for workers in [0, 1, 3] {
                let decoded = parallel(&stream, workers, 1 << 16, 65536);
                assert!(
                    decoded.as_ref().ok() == Some(&data),
                    "block {block} workers {workers}: {:?}",
                    decoded.err()
                );
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Joined {
        One,
        Three,
    }

    struct DamagedStream {
        stream: Vec<u8>,
        flips: Vec<usize>,
    }

    fn damaged_stream(joined: Joined) -> DamagedStream {
        let mut noise = Noise(24);
        let data = sample(1, 250_000, &mut noise);
        let one = compress(&data, 1);
        let one_flips = flipped_bits(&one, &mut noise);
        match joined {
            Joined::One => DamagedStream {
                stream: one,
                flips: one_flips,
            },
            Joined::Three => {
                let stream: Vec<u8> = data
                    .chunks(90_000)
                    .flat_map(|part| compress(part, 1))
                    .collect();
                let flips = flipped_bits(&stream, &mut noise);
                DamagedStream { stream, flips }
            }
        }
    }

    fn flipped_bits(stream: &[u8], noise: &mut Noise) -> Vec<usize> {
        let stream_ends = (1..stream.len() - 4)
            .filter(|&byte| stream[byte..].starts_with(b"BZh1"))
            .chain([stream.len()])
            .flat_map(|byte| byte * 8 - 100..(byte * 8 + 100).min(stream.len() * 8));
        (0..2400)
            .chain(stream_ends)
            .chain((0..3000).map(|_| noise.below(stream.len() * 8)))
            .collect()
    }

    fn cuts_are_refused_serially_and_in_parallel(joined: Joined) {
        let DamagedStream { stream, .. } = damaged_stream(joined);
        let cuts = (0..stream.len())
            .filter(|cut| *cut < 400 || *cut + 400 > stream.len() || cut % 37 == 0);
        for cut in cuts {
            assert_eq!(serial(&stream[..cut]), None);
            for workers in [0, 1, 3] {
                let decoded = parallel(&stream[..cut], workers, 4096, 65536);
                assert!(decoded.is_err(), "cut {cut} workers {workers}");
            }
        }
    }

    const FLIP_PARTS: usize = 3;

    fn flip_part(flips: usize, part: usize) -> impl Iterator<Item = usize> {
        (part..flips).step_by(FLIP_PARTS)
    }

    #[test]
    fn the_flip_parts_hold_every_flip_of_both_streams_once() {
        for joined in [Joined::One, Joined::Three] {
            let flips = damaged_stream(joined).flips.len();
            let mut held: Vec<usize> = (0..FLIP_PARTS)
                .flat_map(|part| flip_part(flips, part))
                .collect();
            held.sort_unstable();
            assert_eq!(held, (0..flips).collect::<Vec<_>>());
        }
    }

    fn flips_give_the_serial_bytes_or_are_refused_as_it_refuses(joined: Joined, part: usize) {
        let DamagedStream { stream, flips } = damaged_stream(joined);
        for index in flip_part(flips.len(), part) {
            let bit = flips[index];
            let mut flipped = stream.clone();
            flipped[bit / 8] ^= 0x80 >> (bit % 8);
            let expected = serial(&flipped);
            for workers in [0, 1, 3] {
                assert_eq!(
                    parallel(&flipped, workers, 4096, 65536).ok(),
                    expected,
                    "bit {bit} workers {workers}"
                );
            }
        }
    }

    #[test]
    fn cuts_of_one_stream_are_refused_serially_and_in_parallel() {
        cuts_are_refused_serially_and_in_parallel(Joined::One);
    }

    #[test]
    fn cuts_of_three_joined_streams_are_refused_serially_and_in_parallel() {
        cuts_are_refused_serially_and_in_parallel(Joined::Three);
    }

    #[test]
    fn every_third_flip_from_the_first_in_one_stream_gives_the_serial_bytes_or_is_refused() {
        flips_give_the_serial_bytes_or_are_refused_as_it_refuses(Joined::One, 0);
    }

    #[test]
    fn every_third_flip_from_the_second_in_one_stream_gives_the_serial_bytes_or_is_refused() {
        flips_give_the_serial_bytes_or_are_refused_as_it_refuses(Joined::One, 1);
    }

    #[test]
    fn every_third_flip_from_the_third_in_one_stream_gives_the_serial_bytes_or_is_refused() {
        flips_give_the_serial_bytes_or_are_refused_as_it_refuses(Joined::One, 2);
    }

    #[test]
    fn every_third_flip_from_the_first_in_three_streams_gives_the_serial_bytes_or_is_refused() {
        flips_give_the_serial_bytes_or_are_refused_as_it_refuses(Joined::Three, 0);
    }

    #[test]
    fn every_third_flip_from_the_second_in_three_streams_gives_the_serial_bytes_or_is_refused() {
        flips_give_the_serial_bytes_or_are_refused_as_it_refuses(Joined::Three, 1);
    }

    #[test]
    fn every_third_flip_from_the_third_in_three_streams_gives_the_serial_bytes_or_is_refused() {
        flips_give_the_serial_bytes_or_are_refused_as_it_refuses(Joined::Three, 2);
    }

    #[test]
    fn a_block_may_fill_the_longest_block_and_one_bit_more_is_refused() {
        let mut noise = Noise(25);
        let (data, stream) = four_blocks(0, &mut noise);
        for planted in [false, true] {
            let stream = if planted {
                with_spare_bits(&stream, 0, Spare::Selectors, &magic_bits(BLOCK_MAGIC))
            } else {
                stream.clone()
            };
            let limit = stream.len() as u64 * 8;
            let block_bits =
                decode_block(&stream, 32, limit, 1, Walk::Onward, &mut Scratch::default())
                    .ok()
                    .unwrap()
                    .end
                    - 32;
            let room = LONGEST_BLOCK as u64 * 8 - block_bits;
            let steps = |bits: u64| {
                std::iter::repeat_n([true, false, true, true], (bits / 4) as usize)
                    .flatten()
                    .collect::<Vec<_>>()
            };
            let fits = with_spare_bits(&stream, 0, Spare::LengthSteps, &steps(room));
            let too_long = with_spare_bits(&stream, 0, Spare::LengthSteps, &steps(room + 4));
            assert_eq!(serial(&too_long).as_ref(), Some(&data));
            for workers in [0, 1, 3] {
                let decoded = parallel(&fits, workers, 1 << 16, 65536);
                assert!(decoded.ok().as_ref() == Some(&data), "{planted} {workers}");
                let refused = parallel(&too_long, workers, 1 << 16, 65536);
                let message = refused.err().map(|error| error.to_string());
                assert!(
                    message
                        .as_ref()
                        .is_some_and(|text| text.contains("compressed bytes")),
                    "{planted} {workers} {message:?}"
                );
            }
        }
    }

    #[test]
    fn blocks_in_flight_stay_within_the_workers_and_their_memory() {
        let mut noise = Noise(26);
        let data = sample(0, 1_500_000, &mut noise);
        let one_stream = compress(&data, 1);
        let a_stream_a_block: Vec<u8> = data
            .chunks(100_000)
            .flat_map(|block| compress(block, 1))
            .collect();
        for stream in [&one_stream, &a_stream_a_block] {
            for (workers, budget, most_expected) in [(4, u64::MAX, 4), (4, 0, 1), (2, u64::MAX, 2)]
            {
                let mut reader = reader(stream, workers, 1 << 16);
                reader.budget = reader.budget.min(budget);
                let (mut most, mut piece, mut decoded) = (0, vec![0u8; 4096], Vec::new());
                loop {
                    let read = reader.read(&mut piece).unwrap();
                    most = most.max(reader.blocks_queued);
                    assert!(reader.charged <= reader.budget.max(PER_WORKER));
                    assert!(reader.queue.len() <= 2 * workers);
                    assert!(
                        reader.held.len() as u64
                            <= 2 * (reader.budget.max(PER_WORKER) + LONGEST_BLOCK as u64)
                    );
                    if read == 0 {
                        break;
                    }
                    decoded.extend_from_slice(&piece[..read]);
                }
                assert!(decoded == data);
                assert_eq!(most, most_expected, "{workers} {budget}");
            }
        }
    }

    #[test]
    fn a_failed_reader_stays_failed_and_a_dropped_one_joins_its_workers() {
        let mut noise = Noise(27);
        let (_, mut stream) = four_blocks(0, &mut noise);
        let mut early = reader(&stream, 4, 1 << 16);
        assert_eq!(early.read(&mut [0u8; 10]).unwrap(), 10);
        assert!(!early.queue.is_empty());
        drop(early);
        let middle = stream.len() / 2;
        stream[middle] ^= 1;
        let mut reader = reader(&stream, 4, 1 << 16);
        let mut piece = vec![0u8; 1 << 16];
        while reader.read(&mut piece).is_ok() {}
        assert!(reader.read(&mut piece).is_err());
    }
}
