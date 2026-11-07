use std::io::{self, Read};

const FILTER_DELTA: u64 = 0x03;
const FILTER_X86: u64 = 0x04;
const FILTER_POWERPC: u64 = 0x05;
const FILTER_IA64: u64 = 0x06;
const FILTER_ARM: u64 = 0x07;
const FILTER_ARM_THUMB: u64 = 0x08;
const FILTER_SPARC: u64 = 0x09;
const FILTER_ARM64: u64 = 0x0A;
const FILTER_RISCV: u64 = 0x0B;
pub(crate) const DELTA_HISTORY: usize = 256;
pub(crate) const BRANCH_BUFFER: usize = 64 * 1024;
const X86_INSTRUCTION: usize = 5;
const IA64_BUNDLE: usize = 16;
const RISCV_PAIR: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Branch {
    X86,
    PowerPc,
    Ia64,
    Arm,
    ArmThumb,
    Arm64,
    Sparc,
    RiscV,
}

impl Branch {
    // SPEC(xz-file-format.txt 5.3.2): a start offset must be a multiple of the filter's
    // alignment, or the decoder must report an error.
    pub(crate) fn alignment(self) -> u32 {
        match self {
            Branch::X86 => 1,
            Branch::ArmThumb | Branch::RiscV => 2,
            Branch::Ia64 => 16,
            Branch::PowerPc | Branch::Arm | Branch::Arm64 | Branch::Sparc => 4,
        }
    }
}

#[derive(Debug)]
struct X86State {
    previous_mask: u32,
    previous_position: u32,
}

#[derive(Debug)]
pub(crate) struct BranchDecoder {
    branch: Branch,
    position: u32,
    x86: X86State,
}

impl BranchDecoder {
    pub(crate) fn new(branch: Branch, start: u32) -> Self {
        BranchDecoder {
            branch,
            position: start,
            x86: X86State {
                previous_mask: 0,
                previous_position: 0u32.wrapping_sub(X86_INSTRUCTION as u32),
            },
        }
    }

    pub(crate) fn convert_final_prefix(&mut self, buffer: &mut [u8]) -> usize {
        let position = self.position;
        let done = match self.branch {
            Branch::X86 => x86(&mut self.x86, position, buffer),
            Branch::PowerPc => powerpc(position, buffer),
            Branch::Ia64 => ia64(position, buffer),
            Branch::Arm => arm(position, buffer),
            Branch::ArmThumb => arm_thumb(position, buffer),
            Branch::Arm64 => arm64(position, buffer),
            Branch::Sparc => sparc(position, buffer),
            Branch::RiscV => riscv(position, buffer),
        };
        self.position = position.wrapping_add(done as u32);
        done
    }
}

fn is_x86_address_top(byte: u8) -> bool {
    byte == 0x00 || byte == 0xFF
}

// EXTERNAL(liblzma simple/x86.c, x86_code): E8 and E9 with a 32-bit operand whose top byte is
// 00 or FF, skipping operands that overlap a recent E8 or E9 as the mask records.
fn x86(state: &mut X86State, position: u32, buffer: &mut [u8]) -> usize {
    const MASK_TO_BIT_NUMBER: [u32; 5] = [0, 1, 2, 2, 3];
    if buffer.len() < X86_INSTRUCTION {
        return 0;
    }
    let mut mask = state.previous_mask;
    let mut previous = state.previous_position;
    if position.wrapping_sub(previous) > X86_INSTRUCTION as u32 {
        previous = position.wrapping_sub(X86_INSTRUCTION as u32);
    }
    let limit = buffer.len() - X86_INSTRUCTION;
    let mut at = 0;
    while at <= limit {
        if buffer[at] & 0xFE != 0xE8 {
            at += 1;
            continue;
        }
        let here = position.wrapping_add(at as u32);
        let offset = here.wrapping_sub(previous);
        previous = here;
        if offset > X86_INSTRUCTION as u32 {
            mask = 0;
        } else {
            for _ in 0..offset {
                mask = (mask & 0x77) << 1;
            }
        }
        let top = buffer[at + 4];
        if is_x86_address_top(top) && mask >> 1 <= 4 && mask >> 1 != 3 {
            let operand: [u8; 4] = buffer[at + 1..at + 5].try_into().unwrap_or_default();
            let mut source = u32::from_le_bytes(operand);
            let next = here.wrapping_add(X86_INSTRUCTION as u32);
            let target = loop {
                let target = source.wrapping_sub(next);
                if mask == 0 {
                    break target;
                }
                let shift = MASK_TO_BIT_NUMBER[(mask >> 1) as usize] * 8;
                if !is_x86_address_top((target >> (24 - shift)) as u8) {
                    break target;
                }
                source = target ^ (u32::MAX >> shift);
            };
            let top = if target & (1 << 24) != 0 { 0xFF } else { 0x00 };
            buffer[at + 1..at + 4].copy_from_slice(&target.to_le_bytes()[..3]);
            buffer[at + 4] = top;
            at += X86_INSTRUCTION;
            mask = 0;
        } else {
            at += 1;
            mask |= 1;
            if is_x86_address_top(top) {
                mask |= 0x10;
            }
        }
    }
    state.previous_mask = mask;
    state.previous_position = previous;
    at
}

fn whole_words(buffer: &mut [u8], unit: usize) -> &mut [u8] {
    let whole = buffer.len() - buffer.len() % unit;
    &mut buffer[..whole]
}

fn word_at(buffer: &[u8], at: usize) -> [u8; 4] {
    buffer[at..at + 4].try_into().unwrap_or_default()
}

// EXTERNAL(liblzma simple/powerpc.c): big-endian `bl` with opcode 18, AA clear and LK set.
fn powerpc(position: u32, buffer: &mut [u8]) -> usize {
    let words = whole_words(buffer, 4);
    for (at, word) in words.chunks_exact_mut(4).enumerate() {
        if word[0] >> 2 != 0x12 || word[3] & 3 != 1 {
            continue;
        }
        let source = u32::from_be_bytes([word[0] & 3, word[1], word[2], word[3] & !3]);
        let target = source.wrapping_sub(position.wrapping_add((at * 4) as u32));
        let [high, upper, lower, low] = target.to_be_bytes();
        word[0] = 0x48 | (high & 3);
        word[1] = upper;
        word[2] = lower;
        word[3] = (word[3] & 3) | low;
    }
    words.len()
}

// EXTERNAL(liblzma simple/ia64.c): in each 16-byte bundle, the slots the template marks as
// branches hold a 41-bit instruction; opcode 5 with btype 0 has a 21-bit bundle offset.
fn ia64(position: u32, buffer: &mut [u8]) -> usize {
    const BRANCH_SLOTS: [u32; 32] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 6, 6, 0, 0, 7, 7, 4, 4, 0, 0, 4, 4,
        0, 0,
    ];
    let bundles = whole_words(buffer, IA64_BUNDLE);
    let length = bundles.len();
    for (index, bundle) in bundles.chunks_exact_mut(IA64_BUNDLE).enumerate() {
        let slots = BRANCH_SLOTS[usize::from(bundle[0] & 0x1F)];
        let here = position.wrapping_add((index * IA64_BUNDLE) as u32);
        for slot in 0..3 {
            if (slots >> slot) & 1 == 0 {
                continue;
            }
            let bit = 5 + 41 * slot as usize;
            let (byte, shift) = (bit / 8, bit % 8);
            let mut field = [0u8; 8];
            field[..6].copy_from_slice(&bundle[byte..byte + 6]);
            let instruction = u64::from_le_bytes(field);
            let mut normal = instruction >> shift;
            if (normal >> 37) & 0xF != 0x5 || (normal >> 9) & 0x7 != 0 {
                continue;
            }
            let mut source = ((normal >> 13) & 0xF_FFFF) as u32;
            source |= (((normal >> 36) & 1) as u32) << 20;
            let target = (source << 4).wrapping_sub(here) >> 4;
            normal &= !(0x8F_FFFFu64 << 13);
            normal |= u64::from(target & 0xF_FFFF) << 13;
            normal |= u64::from(target & 0x10_0000) << (36 - 20);
            let kept = instruction & ((1u64 << shift) - 1);
            let rebuilt = kept | (normal << shift);
            bundle[byte..byte + 6].copy_from_slice(&rebuilt.to_le_bytes()[..6]);
        }
    }
    length
}

// EXTERNAL(liblzma simple/arm.c): little-endian `bl` with condition AL, a 24-bit word offset
// from the instruction plus 8.
fn arm(position: u32, buffer: &mut [u8]) -> usize {
    let words = whole_words(buffer, 4);
    for (at, word) in words.chunks_exact_mut(4).enumerate() {
        if word[3] != 0xEB {
            continue;
        }
        let source = u32::from_le_bytes([word[0], word[1], word[2], 0]) << 2;
        let pc = position.wrapping_add((at * 4) as u32).wrapping_add(8);
        let target = source.wrapping_sub(pc) >> 2;
        word[..3].copy_from_slice(&target.to_le_bytes()[..3]);
    }
    words.len()
}

// EXTERNAL(liblzma simple/armthumb.c): a Thumb `bl` pair, F000 then F800 halfwords, with a
// 22-bit halfword offset from the instruction plus 4. A converted pair is skipped whole.
fn arm_thumb(position: u32, buffer: &mut [u8]) -> usize {
    if buffer.len() < 4 {
        return 0;
    }
    let last = buffer.len() - 4;
    let mut at = 0;
    while at <= last {
        if buffer[at + 1] & 0xF8 == 0xF0 && buffer[at + 3] & 0xF8 == 0xF8 {
            let source = (u32::from(buffer[at + 1] & 7) << 19)
                | (u32::from(buffer[at]) << 11)
                | (u32::from(buffer[at + 3] & 7) << 8)
                | u32::from(buffer[at + 2]);
            let pc = position.wrapping_add(at as u32).wrapping_add(4);
            let target = (source << 1).wrapping_sub(pc) >> 1;
            buffer[at + 1] = 0xF0 | ((target >> 19) & 7) as u8;
            buffer[at] = (target >> 11) as u8;
            buffer[at + 3] = 0xF8 | ((target >> 8) & 7) as u8;
            buffer[at + 2] = target as u8;
            at += 2;
        }
        at += 2;
    }
    at
}

// EXTERNAL(liblzma simple/sparc.c): big-endian `call` whose 30-bit displacement is a sign
// extension of its low 23 bits.
fn sparc(position: u32, buffer: &mut [u8]) -> usize {
    let words = whole_words(buffer, 4);
    for (at, word) in words.chunks_exact_mut(4).enumerate() {
        let call = (word[0] == 0x40 && word[1] & 0xC0 == 0x00)
            || (word[0] == 0x7F && word[1] & 0xC0 == 0xC0);
        if !call {
            continue;
        }
        let source = u32::from_be_bytes(word_at(word, 0)) << 2;
        let target = source.wrapping_sub(position.wrapping_add((at * 4) as u32)) >> 2;
        let sign = (0u32.wrapping_sub((target >> 22) & 1) << 22) & 0x3FFF_FFFF;
        let target = sign | (target & 0x3F_FFFF) | 0x4000_0000;
        word.copy_from_slice(&target.to_be_bytes());
    }
    words.len()
}

// EXTERNAL(liblzma simple/arm64.c): BL with its whole 26-bit offset, and ADRP when its page
// offset lies within +/-512 MiB.
fn arm64(position: u32, buffer: &mut [u8]) -> usize {
    let words = whole_words(buffer, 4);
    for (at, word) in words.chunks_exact_mut(4).enumerate() {
        let pc = position.wrapping_add((at * 4) as u32);
        let instruction = u32::from_le_bytes(word_at(word, 0));
        let decoded = if instruction >> 26 == 0x25 {
            0x9400_0000 | (instruction.wrapping_sub(pc >> 2) & 0x03FF_FFFF)
        } else if instruction & 0x9F00_0000 == 0x9000_0000 {
            let source = ((instruction >> 29) & 3) | ((instruction >> 3) & 0x001F_FFFC);
            if source.wrapping_add(0x0002_0000) & 0x001C_0000 != 0 {
                continue;
            }
            let target = source.wrapping_sub(pc >> 12);
            (instruction & 0x9000_001F)
                | ((target & 3) << 29)
                | ((target & 0x0003_FFFC) << 3)
                | (0u32.wrapping_sub(target & 0x0002_0000) & 0x00E0_0000)
        } else {
            continue;
        };
        word.copy_from_slice(&decoded.to_le_bytes());
    }
    words.len()
}

// EXTERNAL(liblzma simple/riscv.c, NOT_AUIPC_PAIR): an AUIPC whose rd differs from the next
// instruction's rs1, or whose next instruction is compressed, is not a pair.
fn not_auipc_pair(auipc: u32, next: u32) -> bool {
    ((auipc << 8) ^ next.wrapping_sub(3)) & 0xF_8003 != 0
}

// EXTERNAL(liblzma simple/riscv.c, NOT_SPECIAL_AUIPC): the encoder's swapped form is an AUIPC
// with rd x2, bits 12 and 13 set, and a packed rs1 other than x0 or x2.
fn not_special_auipc(auipc: u32, packed_rs1: u32) -> bool {
    (auipc.wrapping_sub(0x3117) << 18) >= (packed_rs1 & 0x1D)
}

// EXTERNAL(liblzma simple/riscv.c, riscv_decode): JAL with rd x1 or x5, and AUIPC with its
// paired instruction, which the encoder stores in a swapped form.
fn riscv(position: u32, buffer: &mut [u8]) -> usize {
    if buffer.len() < RISCV_PAIR {
        return 0;
    }
    let last = buffer.len() - RISCV_PAIR;
    let mut at = 0;
    while at <= last {
        let first = buffer[at];
        if first == 0xEF {
            let b1 = u32::from(buffer[at + 1]);
            if b1 & 0x0D == 0 {
                let b2 = u32::from(buffer[at + 2]);
                let b3 = u32::from(buffer[at + 3]);
                let pc = position.wrapping_add(at as u32);
                let address = (((b1 & 0xF0) << 13) | (b2 << 9) | (b3 << 1)).wrapping_sub(pc);
                buffer[at + 1] = ((b1 & 0x0F) | ((address >> 8) & 0xF0)) as u8;
                buffer[at + 2] = (((address >> 16) & 0x0F)
                    | ((address >> 7) & 0x10)
                    | ((address << 4) & 0xE0)) as u8;
                buffer[at + 3] = (((address >> 4) & 0x7F) | ((address >> 13) & 0x80)) as u8;
                at += 2;
            }
        } else if first & 0x7F == 0x17 {
            let auipc = u32::from_le_bytes(word_at(buffer, at));
            let pair = if auipc & 0xE80 != 0 {
                let next = u32::from_le_bytes(word_at(buffer, at + 4));
                if not_auipc_pair(auipc, next) {
                    at += 6;
                    continue;
                }
                let address = (auipc & 0xFFFF_F000).wrapping_add(next >> 20);
                (0x17 | (2 << 7) | (next << 12), address)
            } else {
                let packed_rs1 = auipc >> 27;
                if not_special_auipc(auipc, packed_rs1) {
                    at += 4;
                    continue;
                }
                let address = u32::from_be_bytes(word_at(buffer, at + 4))
                    .wrapping_sub(position.wrapping_add(at as u32));
                let next = (auipc >> 12) | (address << 20);
                let rebuilt =
                    0x17 | (packed_rs1 << 7) | (address.wrapping_add(0x800) & 0xFFFF_F000);
                (rebuilt, next)
            };
            buffer[at..at + 4].copy_from_slice(&pair.0.to_le_bytes());
            buffer[at + 4..at + 8].copy_from_slice(&pair.1.to_le_bytes());
            at += 6;
        }
        at += 2;
    }
    at
}

#[derive(Debug)]
pub(crate) struct DeltaDecoder {
    distance: usize,
    position: u8,
    history: Box<[u8; DELTA_HISTORY]>,
}

impl DeltaDecoder {
    pub(crate) fn new(distance: usize) -> Self {
        DeltaDecoder {
            distance,
            position: 0,
            history: Box::new([0; DELTA_HISTORY]),
        }
    }

    // SPEC(xz-file-format.txt 5.3.3.1): each byte adds the decoded byte `distance` places
    // back, kept in a 256-byte ring indexed by a position that counts down.
    pub(crate) fn convert(&mut self, buffer: &mut [u8]) {
        for byte in buffer {
            let back = (self.distance + usize::from(self.position)) % DELTA_HISTORY;
            *byte = byte.wrapping_add(self.history[back]);
            self.history[usize::from(self.position)] = *byte;
            self.position = self.position.wrapping_sub(1);
        }
    }
}

#[derive(Debug)]
pub(crate) struct BranchStage {
    decoder: BranchDecoder,
    buffer: Box<[u8]>,
    start: usize,
    converted: usize,
    end: usize,
    input_ended: bool,
}

impl BranchStage {
    fn new(decoder: BranchDecoder) -> Self {
        BranchStage {
            decoder,
            buffer: vec![0; BRANCH_BUFFER].into_boxed_slice(),
            start: 0,
            converted: 0,
            end: 0,
            input_ended: false,
        }
    }

    fn read(&mut self, inner: &mut dyn Read, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            if self.start < self.converted {
                let count = out.len().min(self.converted - self.start);
                out[..count].copy_from_slice(&self.buffer[self.start..self.start + count]);
                self.start += count;
                return Ok(count);
            }
            if self.input_ended {
                if self.converted == self.end {
                    return Ok(0);
                }
                self.converted = self.end;
                continue;
            }
            self.buffer.copy_within(self.converted..self.end, 0);
            self.end -= self.converted;
            self.start = 0;
            self.converted = 0;
            let read = inner.read(&mut self.buffer[self.end..])?;
            if read == 0 {
                self.input_ended = true;
                continue;
            }
            self.end += read;
            self.converted = self
                .decoder
                .convert_final_prefix(&mut self.buffer[..self.end]);
        }
    }
}

#[derive(Debug)]
pub(crate) enum Filter {
    Branch(BranchStage),
    Delta(DeltaDecoder),
}

impl Filter {
    pub(crate) fn branch(branch: Branch, start: u32) -> Self {
        Filter::Branch(BranchStage::new(BranchDecoder::new(branch, start)))
    }

    pub(crate) fn read(&mut self, inner: &mut dyn Read, out: &mut [u8]) -> io::Result<usize> {
        match self {
            Filter::Branch(stage) => stage.read(inner, out),
            Filter::Delta(delta) => {
                let read = inner.read(out)?;
                delta.convert(&mut out[..read]);
                Ok(read)
            }
        }
    }
}

// SPEC(xz-file-format.txt 5.3.2 and 5.3.3): Branch/Call/Jump filters take no properties or a
// 4-byte little-endian start offset; Delta takes one byte, the distance less one.
pub(crate) fn xz_filter(id: u64, properties: &[u8]) -> Result<Filter, &'static str> {
    let branch = match id {
        FILTER_DELTA => {
            return match *properties {
                [distance] => Ok(Filter::Delta(DeltaDecoder::new(usize::from(distance) + 1))),
                _ => Err("xz Delta filter properties are not one byte"),
            };
        }
        FILTER_X86 => Branch::X86,
        FILTER_POWERPC => Branch::PowerPc,
        FILTER_IA64 => Branch::Ia64,
        FILTER_ARM => Branch::Arm,
        FILTER_ARM_THUMB => Branch::ArmThumb,
        FILTER_SPARC => Branch::Sparc,
        FILTER_ARM64 => Branch::Arm64,
        FILTER_RISCV => Branch::RiscV,
        _ => return Err("xz filter is unknown"),
    };
    let start = match *properties {
        [] => 0,
        [a, b, c, d] => u32::from_le_bytes([a, b, c, d]),
        _ => return Err("xz branch filter properties are not 0 or 4 bytes"),
    };
    if !start.is_multiple_of(branch.alignment()) {
        return Err("xz branch filter start offset is not a multiple of its alignment");
    }
    Ok(Filter::branch(branch, start))
}

struct Rest<'a, R> {
    filters: &'a mut [Filter],
    last: &'a mut R,
}

impl<R: Read> Read for Rest<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        read_through(self.filters, self.last, out)
    }
}

pub(crate) fn read_through<R: Read>(
    filters: &mut [Filter],
    last: &mut R,
    out: &mut [u8],
) -> io::Result<usize> {
    match filters.split_first_mut() {
        None => last.read(out),
        Some((first, filters)) => first.read(&mut Rest { filters, last }, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lzma::Lzma2Reader;
    use liblzma::stream::{Action, Filters, LzmaOptions, Status, Stream};
    use std::time::Instant;

    const DICTIONARY: u32 = 4096;
    const STORED_CHUNK: usize = 1 << 16;
    const IA64_SLOT_BITS: u32 = 41;
    const IA64_BRANCH_OPCODE: u64 = 5;

    struct Case {
        name: &'static str,
        id: u64,
        alphabet: &'static [u8],
        longest: usize,
        units: Vec<Vec<u8>>,
        most_units: usize,
        starts: Vec<u32>,
    }

    fn ids() -> [(u64, &'static str); 9] {
        [
            (FILTER_X86, "x86"),
            (FILTER_POWERPC, "PowerPC"),
            (FILTER_IA64, "IA-64"),
            (FILTER_ARM, "ARM"),
            (FILTER_ARM_THUMB, "ARM-Thumb"),
            (FILTER_SPARC, "SPARC"),
            (FILTER_ARM64, "ARM64"),
            (FILTER_RISCV, "RISC-V"),
            (FILTER_DELTA, "Delta"),
        ]
    }

    fn ia64_bundle(template: u8, slots: [u64; 3]) -> Vec<u8> {
        let mut bits = u128::from(template & 0x1F);
        for (slot, instruction) in slots.into_iter().enumerate() {
            let mask = (1u128 << IA64_SLOT_BITS) - 1;
            bits |= (u128::from(instruction) & mask) << (5 + IA64_SLOT_BITS * slot as u32);
        }
        bits.to_le_bytes().to_vec()
    }

    fn ia64_branch(offset: u32, btype: u64) -> u64 {
        (IA64_BRANCH_OPCODE << 37)
            | (u64::from(offset >> 20 & 1) << 36)
            | (u64::from(offset & 0xF_FFFF) << 13)
            | (btype << 9)
    }

    fn ia64_units() -> Vec<Vec<u8>> {
        let branch = ia64_branch(0x12345, 0);
        let backward = ia64_branch(0x1F_FFF0, 0);
        let other_type = ia64_branch(0x12345, 1);
        let other_opcode = (4 << 37) | (0x12345 << 13);
        vec![
            ia64_bundle(0x10, [0, 0, branch]),
            ia64_bundle(0x12, [0, branch, backward]),
            ia64_bundle(0x16, [backward, branch, branch]),
            ia64_bundle(0x18, [branch, 0, other_type]),
            ia64_bundle(0x1C, [0, 0, other_opcode]),
            ia64_bundle(0x11, [branch, branch, branch]),
            ia64_bundle(0x00, [branch, branch, branch]),
            vec![0xFF; 16],
        ]
    }

    fn riscv_units() -> Vec<Vec<u8>> {
        [
            0x0000_00EFu32,
            0x8000_02EF,
            0x0000_006F,
            0x0001_2097,
            0x0000_0297,
            0xFFFF_F517,
            0x0000_80E7,
            0x0285_0513,
            0x0002_A283,
            0x0005_3503,
            0x00A5_3023,
            0x0000_3117,
            0xFFFF_FFFF,
            0,
        ]
        .into_iter()
        .map(|word| word.to_le_bytes().to_vec())
        .chain([vec![0x01, 0x00]])
        .collect()
    }

    fn cases() -> Vec<Case> {
        let words = |words: &[u32], big_endian: bool| -> Vec<Vec<u8>> {
            words
                .iter()
                .map(|&word| {
                    if big_endian {
                        word.to_be_bytes().to_vec()
                    } else {
                        word.to_le_bytes().to_vec()
                    }
                })
                .collect()
        };
        let case = |name, id, alphabet, longest, units, most_units, starts| Case {
            name,
            id,
            alphabet,
            longest,
            units,
            most_units,
            starts,
        };
        vec![
            case(
                "x86",
                FILTER_X86,
                &[0x00, 0xE8, 0xE9, 0xFF, 0x7F][..],
                7,
                vec![
                    vec![0xE8, 0x10, 0x00, 0x00, 0x00],
                    vec![0xE8, 0xF0, 0xFF, 0xFF, 0xFF],
                    vec![0xE9, 0x00, 0x00, 0x00, 0x00],
                    vec![0xE8, 0x00, 0x00, 0x00, 0x01],
                    vec![0x90],
                    vec![0xFF],
                ],
                4,
                vec![0, 1, 0xFFFF_FFFB, 0x8000_0003],
            ),
            case(
                "PowerPC",
                FILTER_POWERPC,
                &[0x48, 0x4B, 0x01, 0xFF],
                8,
                words(&[0x4800_0101, 0x4BFF_FFFD, 0x4800_0103, 0x6000_0000], true),
                3,
                vec![0, 4, 0xFFFF_FFFC],
            ),
            case(
                "IA-64",
                FILTER_IA64,
                &[0x00, 0x10, 0x50, 0xFF],
                6,
                ia64_units(),
                3,
                vec![0, 16, 0xFFFF_FFF0],
            ),
            case(
                "ARM",
                FILTER_ARM,
                &[0xEB, 0x00, 0xFF, 0x12],
                8,
                words(&[0xEB00_0010, 0xEBFF_FFFE, 0xEA00_0010, 0xE1A0_0000], false),
                3,
                vec![0, 4, 0xFFFF_FFFC],
            ),
            case(
                "ARM-Thumb",
                FILTER_ARM_THUMB,
                &[0xF0, 0xF8, 0xFF, 0x00],
                8,
                vec![
                    vec![0x00, 0xF0, 0x10, 0xF8],
                    vec![0xFF, 0xF7, 0xFE, 0xFF],
                    vec![0x00, 0xF0],
                    vec![0x00, 0xBF],
                ],
                4,
                vec![0, 2, 0xFFFF_FFFE],
            ),
            case(
                "SPARC",
                FILTER_SPARC,
                &[0x40, 0x7F, 0x00, 0xFF],
                8,
                words(&[0x4000_0010, 0x7FFF_FFFE, 0x4040_0000, 0x0100_0000], true),
                3,
                vec![0, 4, 0xFFFF_FFFC],
            ),
            case(
                "ARM64",
                FILTER_ARM64,
                &[0x94, 0x90, 0x00, 0xFF],
                8,
                words(
                    &[
                        0x9400_0010,
                        0x97FF_FFFE,
                        0x9000_0010,
                        0xF0FF_FFE0,
                        0x9020_0000,
                        0xD503_201F,
                    ],
                    false,
                ),
                3,
                vec![0, 4, 0x0000_1000, 0xFFFF_FFFC],
            ),
            case(
                "RISC-V",
                FILTER_RISCV,
                &[0xEF, 0x97, 0x00, 0x02],
                8,
                riscv_units(),
                3,
                vec![0, 2, 0xFFFF_FFFE],
            ),
            case(
                "Delta",
                FILTER_DELTA,
                &[0x00, 0x01, 0x80, 0xFF],
                7,
                vec![vec![1, 2, 3], vec![0xFF], vec![0x80, 0x80]],
                3,
                vec![0, 1, 2, 3, 6, 255],
            ),
        ]
    }

    fn properties(case: &Case, start: u32) -> Vec<u8> {
        if case.id == FILTER_DELTA {
            vec![start as u8]
        } else if start == 0 {
            Vec::new()
        } else {
            start.to_le_bytes().to_vec()
        }
    }

    fn liblzma_chain(id: u64, properties: &[u8]) -> Result<Filters, liblzma::stream::Error> {
        let mut filters = Filters::new();
        match id {
            FILTER_DELTA => filters.delta_properties(properties),
            FILTER_X86 => filters.x86_properties(properties),
            FILTER_POWERPC => filters.powerpc_properties(properties),
            FILTER_IA64 => filters.ia64_properties(properties),
            FILTER_ARM => filters.arm_properties(properties),
            FILTER_ARM_THUMB => filters.arm_thumb_properties(properties),
            FILTER_SPARC => filters.sparc_properties(properties),
            FILTER_ARM64 => filters.arm64_properties(properties),
            FILTER_RISCV => filters.riscv_properties(properties),
            _ => panic!("not a filter: {id:#x}"),
        }?;
        let mut options = LzmaOptions::new_preset(0).unwrap();
        options.dict_size(DICTIONARY);
        filters.lzma2(&options);
        Ok(filters)
    }

    fn run(stream: &mut Stream, input: &[u8]) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            out.reserve(input.len() + 4096);
            let before = (stream.total_in(), stream.total_out());
            let rest = &input[stream.total_in() as usize..];
            match stream.process_vec(rest, &mut out, Action::Finish) {
                Ok(Status::StreamEnd) => return Some(out),
                Ok(_) if (stream.total_in(), stream.total_out()) != before => {}
                _ => return None,
            }
        }
    }

    fn liblzma_encode(id: u64, properties: &[u8], data: &[u8]) -> Vec<u8> {
        let filters = liblzma_chain(id, properties).unwrap();
        run(&mut Stream::new_raw_encoder(&filters).unwrap(), data).unwrap()
    }

    fn stored_lzma2(data: &[u8]) -> Vec<u8> {
        let mut stream = Vec::with_capacity(data.len() + 16);
        for (index, chunk) in data.chunks(STORED_CHUNK).enumerate() {
            let size = chunk.len() - 1;
            stream.extend([
                if index == 0 { 0x01 } else { 0x02 },
                (size >> 8) as u8,
                size as u8,
            ]);
            stream.extend_from_slice(chunk);
        }
        stream.push(0x00);
        stream
    }

    fn liblzma_decode(id: u64, properties: &[u8], filtered: &[u8]) -> Vec<u8> {
        let filters = liblzma_chain(id, properties).unwrap();
        let mut decoder = Stream::new_raw_decoder(&filters).unwrap();
        run(&mut decoder, &stored_lzma2(filtered)).unwrap()
    }

    struct Pieces<'a> {
        data: &'a [u8],
        cuts: &'a [usize],
        at: usize,
        piece: usize,
    }

    impl Read for Pieces<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let edge = self
                .cuts
                .iter()
                .copied()
                .find(|&cut| cut > self.at)
                .unwrap_or(self.data.len());
            let count = buffer
                .len()
                .min(self.piece)
                .min(edge.min(self.data.len()) - self.at);
            buffer[..count].copy_from_slice(&self.data[self.at..self.at + count]);
            self.at += count;
            Ok(count)
        }
    }

    fn ours(id: u64, properties: &[u8], inner: &mut dyn Read, chunk: usize) -> Vec<u8> {
        let mut filter = xz_filter(id, properties).unwrap();
        let mut out = Vec::new();
        let mut buffer = vec![0u8; chunk];
        loop {
            match filter.read(inner, &mut buffer).unwrap() {
                0 => return out,
                read => out.extend_from_slice(&buffer[..read]),
            }
        }
    }

    fn ours_from(
        id: u64,
        properties: &[u8],
        filtered: &[u8],
        cuts: &[usize],
        piece: usize,
        chunk: usize,
    ) -> Vec<u8> {
        let mut inner = Pieces {
            data: filtered,
            cuts,
            at: 0,
            piece,
        };
        ours(id, properties, &mut inner, chunk)
    }

    fn every_string(alphabet: &[u8], longest: usize, mut visit: impl FnMut(&[u8])) {
        for length in 0..=longest {
            let mut digits = vec![0usize; length];
            loop {
                let input: Vec<u8> = digits.iter().map(|&digit| alphabet[digit]).collect();
                visit(&input);
                let Some(place) = digits.iter().rposition(|&digit| digit + 1 < alphabet.len())
                else {
                    break;
                };
                digits[place] += 1;
                digits[place + 1..].fill(0);
            }
        }
    }

    fn every_unit_sequence(units: &[Vec<u8>], most: usize, mut visit: impl FnMut(&[u8])) {
        for count in 1..=most {
            let mut digits = vec![0usize; count];
            loop {
                let whole: Vec<u8> = digits.iter().flat_map(|&d| units[d].clone()).collect();
                let last = units[digits[count - 1]].len();
                for cut in whole.len() + 1 - last..=whole.len() {
                    visit(&whole[..cut]);
                }
                let Some(place) = digits.iter().rposition(|&digit| digit + 1 < units.len()) else {
                    break;
                };
                digits[place] += 1;
                digits[place + 1..].fill(0);
            }
        }
    }

    fn assert_round_trip(case: &Case, start: u32, data: &[u8], reads: &[(usize, usize)]) -> bool {
        let properties = properties(case, start);
        let stream = liblzma_encode(case.id, &properties, data);
        let mut filtered = Vec::new();
        Lzma2Reader::new(&stream[..], DICTIONARY)
            .read_to_end(&mut filtered)
            .unwrap();
        let what = || format!("{}, start {start:#x}, input {data:02x?}", case.name);
        let mut chained = xz_filter(case.id, &properties).unwrap();
        let mut lzma2 = Lzma2Reader::new(&stream[..], DICTIONARY);
        let mut through = Vec::new();
        let mut one = [0u8; 1];
        while read_through(std::slice::from_mut(&mut chained), &mut lzma2, &mut one).unwrap() == 1 {
            through.push(one[0]);
        }
        assert!(through == data, "{}: through LZMA2 in 1-byte reads", what());
        for &(piece, chunk) in reads {
            let out = ours_from(case.id, &properties, &filtered, &[], piece, chunk);
            assert!(out == data, "{}: pieces {piece}, reads {chunk}", what());
        }
        filtered != data
    }

    fn assert_decodes_as_liblzma(case: &Case, start: u32, filtered: &[u8], every_cut: bool) {
        let properties = properties(case, start);
        let theirs = liblzma_decode(case.id, &properties, filtered);
        let what = || format!("{}, start {start:#x}, filtered {filtered:02x?}", case.name);
        for (piece, chunk) in [(1, 1), (usize::MAX, 1), (1, 3), (usize::MAX, 65_536)] {
            let out = ours_from(case.id, &properties, filtered, &[], piece, chunk);
            assert!(out == theirs, "{}: pieces {piece}, reads {chunk}", what());
        }
        if every_cut {
            for cut in 1..filtered.len() {
                let out = ours_from(case.id, &properties, filtered, &[cut], usize::MAX, 5);
                assert!(out == theirs, "{}: split at {cut}", what());
            }
        }
    }

    #[test]
    fn every_short_input_round_trips_under_liblzmas_encoder() {
        for case in cases() {
            let started = Instant::now();
            let (mut inputs, mut converted) = (0, 0);
            let mut check = |data: &[u8]| {
                inputs += 1;
                converted += usize::from(assert_round_trip(&case, 0, data, &[(1, 7), (3, 1)]));
            };
            every_string(case.alphabet, case.longest, &mut check);
            every_unit_sequence(&case.units, case.most_units, &mut check);
            assert!(converted > 0, "{}: no input was converted", case.name);
            eprintln!(
                "{}: {inputs} inputs, bytes up to {} over {} symbols, up to {} of {} units, \
                 {converted} converted, {:?}",
                case.name,
                case.longest,
                case.alphabet.len(),
                case.most_units,
                case.units.len(),
                started.elapsed()
            );
        }
    }

    #[test]
    fn every_short_input_decodes_as_liblzma_decodes_it_at_every_split_and_start() {
        for case in cases() {
            let started = Instant::now();
            let mut inputs = 0;
            for &start in &case.starts {
                let mut check = |filtered: &[u8]| {
                    inputs += 1;
                    assert_decodes_as_liblzma(&case, start, filtered, true);
                };
                every_string(case.alphabet, case.longest.min(6), &mut check);
                every_unit_sequence(&case.units, case.most_units.min(2), &mut check);
            }
            eprintln!("{}: {inputs} inputs, {:?}", case.name, started.elapsed());
        }
    }

    fn random_input(case: &Case, state: &mut u32, length: usize) -> Vec<u8> {
        let mut next = || {
            *state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            *state >> 8
        };
        let mut out = Vec::with_capacity(length + 16);
        while out.len() < length {
            let pick = next();
            match pick % 4 {
                0 => out.push(pick as u8),
                1 => out.push(case.alphabet[(pick >> 8) as usize % case.alphabet.len()]),
                _ => out.extend_from_slice(&case.units[(pick >> 8) as usize % case.units.len()]),
            }
        }
        out.truncate(length);
        out
    }

    #[test]
    fn pseudo_random_inputs_round_trip_and_decode_as_liblzma_does_at_every_start() {
        let reads = [
            (usize::MAX, 1),
            (7, 13),
            (usize::MAX, 4_093),
            (1_000, 65_536),
        ];
        for case in cases() {
            let mut state = case.id as u32 * 7_919;
            let mut converted = 0;
            for round in 0..120 {
                let length = [1, 15, 16, 17, 100, 777, 4_096, 9_001][round % 8];
                let data = random_input(&case, &mut state, length);
                let start = case.starts[round % case.starts.len()];
                converted += usize::from(assert_round_trip(&case, start, &data, &reads));
                assert_decodes_as_liblzma(&case, start, &data, length <= 100);
            }
            assert!(converted > 40, "{}: only {converted} converted", case.name);
            let large = random_input(&case, &mut state, 3 * BRANCH_BUFFER + 11);
            assert!(assert_round_trip(
                &case,
                case.starts[1],
                &large,
                &reads[2..]
            ));
            let cuts = [BRANCH_BUFFER - 3, BRANCH_BUFFER + 1, 2 * BRANCH_BUFFER - 1];
            let properties = properties(&case, case.starts[1]);
            let theirs = liblzma_decode(case.id, &properties, &large);
            for chunk in [1, 4_093, 1 << 20] {
                let out = ours_from(case.id, &properties, &large, &cuts, usize::MAX, chunk);
                assert!(out == theirs, "{}: large input, reads {chunk}", case.name);
            }
        }
    }

    #[test]
    fn properties_are_accepted_and_refused_as_liblzma_does() {
        let mut lengths = vec![Vec::new()];
        lengths.extend((1..=6).map(|length| vec![0u8; length]));
        let starts = [
            1u32,
            2,
            3,
            4,
            6,
            8,
            12,
            15,
            16,
            17,
            24,
            32,
            0x8000_0000,
            0xFFFF_FFFF,
        ];
        for (id, name) in ids() {
            let mut all: Vec<Vec<u8>> = lengths.clone();
            all.extend(starts.iter().map(|start| start.to_le_bytes().to_vec()));
            all.extend((0..=255).map(|byte| vec![byte]));
            all.push(vec![0x10, 0, 0, 0, 0]);
            let mut accepted = 0;
            for properties in all {
                let theirs = liblzma_chain(id, &properties)
                    .and_then(|filters| Stream::new_raw_decoder(&filters))
                    .is_ok();
                let ours = xz_filter(id, &properties).is_ok();
                assert_eq!(ours, theirs, "{name}, properties {properties:02x?}");
                accepted += usize::from(ours);
            }
            assert!(accepted > 1, "{name}: accepted {accepted}");
        }
        for id in [0x00, 0x01, 0x02, 0x0C, 0x0D, 0x20, 0x21, 0x22, 1 << 40] {
            assert!(xz_filter(id, &[]).is_err(), "{id:#x}");
            assert!(xz_filter(id, &[0]).is_err(), "{id:#x}");
        }
    }

    #[test]
    fn a_delta_filter_adds_the_byte_its_distance_back_across_reads() {
        for distance in [1usize, 2, 7, 255, 256] {
            let data: Vec<u8> = (0..1_000u32).map(|n| (n * 37 % 251) as u8).collect();
            let properties = [(distance - 1) as u8];
            let stream = liblzma_encode(FILTER_DELTA, &properties, &data);
            let mut filtered = Vec::new();
            Lzma2Reader::new(&stream[..], DICTIONARY)
                .read_to_end(&mut filtered)
                .unwrap();
            let expected: Vec<u8> = (0..data.len())
                .map(|at| {
                    let back = at.checked_sub(distance).map_or(0, |back| data[back]);
                    data[at].wrapping_sub(back)
                })
                .collect();
            assert_eq!(filtered, expected, "distance {distance}");
            for chunk in [1, 3, 256, 257] {
                assert_eq!(
                    ours_from(FILTER_DELTA, &properties, &filtered, &[], 5, chunk),
                    data,
                    "distance {distance}, reads {chunk}"
                );
            }
        }
    }

    #[test]
    fn every_ia64_template_and_slot_mix_decodes_as_liblzma_does() {
        let case = cases()
            .into_iter()
            .find(|case| case.id == FILTER_IA64)
            .unwrap();
        let kept_fields = 0x1FF | (0x7 << 33);
        let kinds = [
            0,
            ia64_branch(0x12345, 0),
            ia64_branch(0x1F_FFF0, 0),
            ia64_branch(0x0A5A5, 0) | kept_fields,
            ia64_branch(0x12345, 1),
            (4 << 37) | (0x12345 << 13),
        ];
        let started = Instant::now();
        let (mut bundles, mut converted) = (0, 0);
        for template in 0..32 {
            for first in kinds {
                for second in kinds {
                    for third in kinds {
                        let bundle = ia64_bundle(template, [first, second, third]);
                        bundles += 1;
                        converted += usize::from(assert_round_trip(
                            &case,
                            0x30,
                            &bundle,
                            &[(1, 1), (5, 16)],
                        ));
                        assert_decodes_as_liblzma(&case, 0xFFFF_FFF0, &bundle, false);
                    }
                }
            }
        }
        assert!(converted > 0);
        eprintln!(
            "IA-64: {bundles} bundles, {converted} converted, {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_branch_filter_releases_its_held_bytes_unconverted_at_the_end() {
        let tails: [(u64, &[u8]); 4] = [
            (FILTER_X86, &[0x90, 0xE8, 0x10, 0x00, 0x00]),
            (FILTER_ARM_THUMB, &[0x00, 0x00, 0x00, 0xF0, 0x10]),
            (FILTER_RISCV, &[0x97, 0x02, 0x00, 0x00, 0x83, 0xA2, 0x02]),
            (FILTER_IA64, &ia64_units()[0][..15]),
        ];
        for (id, data) in tails {
            assert_eq!(liblzma_decode(id, &[], data), data, "{id:#x}");
            assert_eq!(ours_from(id, &[], data, &[], 1, 1), data, "{id:#x}");
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    fn converts_a_prefix_and_leaves_the_rest<const LONGEST: usize>(branch: Branch, unit: usize) {
        let mut buffer: [u8; LONGEST] = kani::any();
        let original = buffer;
        let length: usize = kani::any();
        let first_end: usize = kani::any();
        kani::assume(first_end <= length && length <= LONGEST);
        let start: u32 = kani::any();
        let mut decoder = BranchDecoder::new(branch, start);
        let first = decoder.convert_final_prefix(&mut buffer[..first_end]);
        assert!(first <= first_end);
        assert!(buffer[first..] == original[first..]);
        if unit > 0 {
            assert!(first == first_end - first_end % unit);
        }
        let carried = buffer;
        let second = decoder.convert_final_prefix(&mut buffer[first..length]);
        let done = first + second;
        assert!(done <= length);
        assert!(buffer[done..] == carried[done..]);
        assert!(decoder.position == start.wrapping_add(done as u32));
        if unit > 0 {
            assert!(done == length - length % unit);
        }
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn x86_converts_a_prefix_and_leaves_the_rest() {
        converts_a_prefix_and_leaves_the_rest::<12>(Branch::X86, 0);
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn powerpc_converts_whole_words_and_leaves_the_rest() {
        converts_a_prefix_and_leaves_the_rest::<12>(Branch::PowerPc, 4);
    }

    #[kani::proof]
    #[kani::unwind(18)]
    fn ia64_converts_whole_bundles_and_leaves_the_rest() {
        converts_a_prefix_and_leaves_the_rest::<17>(Branch::Ia64, IA64_BUNDLE);
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn arm_converts_whole_words_and_leaves_the_rest() {
        converts_a_prefix_and_leaves_the_rest::<12>(Branch::Arm, 4);
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn arm_thumb_converts_a_prefix_and_leaves_the_rest() {
        converts_a_prefix_and_leaves_the_rest::<12>(Branch::ArmThumb, 0);
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn sparc_converts_whole_words_and_leaves_the_rest() {
        converts_a_prefix_and_leaves_the_rest::<12>(Branch::Sparc, 4);
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn arm64_converts_whole_words_and_leaves_the_rest() {
        converts_a_prefix_and_leaves_the_rest::<12>(Branch::Arm64, 4);
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn riscv_converts_a_prefix_and_leaves_the_rest() {
        converts_a_prefix_and_leaves_the_rest::<12>(Branch::RiscV, 0);
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn delta_adds_the_byte_its_distance_back_for_every_distance() {
        let distance: usize = kani::any();
        kani::assume((1..=DELTA_HISTORY).contains(&distance));
        let mut decoder = DeltaDecoder::new(distance);
        decoder.position = kani::any();
        decoder.history = Box::new(kani::any());
        let first_back = (distance + usize::from(decoder.position)) % DELTA_HISTORY;
        let added = decoder.history[first_back];
        let mut buffer: [u8; 6] = kani::any();
        let original = buffer;
        let length: usize = kani::any();
        kani::assume(length <= buffer.len());
        decoder.convert(&mut buffer[..length]);
        assert!(buffer[length..] == original[length..]);
        if length > 0 {
            assert!(buffer[0] == original[0].wrapping_add(added));
        }
    }
}
