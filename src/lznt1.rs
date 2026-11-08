pub(crate) const CHUNK: usize = 4096;
const SIZE_MASK: u16 = 0x0FFF;
const COMPRESSED: u16 = 0x8000;
const FIRST_DISTANCE_BITS: u32 = 4;
const MIN_MATCH: usize = 3;

// EXTERNAL(7-Zip NtfsHandler Lznt1Dec): decoding stops at a zero chunk header, a cut header,
// a cut chunk and a chunk that ends short of 4 KiB, and returns 0 for a chunk it cannot decode.
pub(crate) fn decode_whole_chunks(unit: &mut [u8], wanted: usize, mut source: &[u8]) -> usize {
    let mut written = 0;
    while written < wanted {
        if source.len() < 2 || written % CHUNK != 0 || written + CHUNK > unit.len() {
            break;
        }
        let header = u16::from_le_bytes([source[0], source[1]]);
        if header == 0 {
            break;
        }
        source = &source[2..];
        let size = usize::from(header & SIZE_MASK) + 1;
        if size > source.len() {
            break;
        }
        let (chunk, rest) = source.split_at(size);
        source = rest;
        let out = &mut unit[written..written + CHUNK];
        if header & COMPRESSED == 0 {
            if size != CHUNK {
                break;
            }
            out.copy_from_slice(chunk);
            written += CHUNK;
            continue;
        }
        match decode_chunk(out, chunk) {
            Some(length) => written += length,
            None => return 0,
        }
    }
    written
}

fn decode_chunk(out: &mut [u8], chunk: &[u8]) -> Option<usize> {
    if chunk[0] & 1 != 0 {
        return None;
    }
    let mut at = 0;
    let mut length = 0;
    let mut distance_bits = FIRST_DISTANCE_BITS;
    while at < chunk.len() {
        let flags = chunk[at];
        at += 1;
        for bit in 0..8 {
            if at == chunk.len() {
                break;
            }
            if flags >> bit & 1 == 0 {
                if length >= CHUNK {
                    return None;
                }
                out[length] = chunk[at];
                at += 1;
                length += 1;
                continue;
            }
            if chunk.len() - at < 2 {
                return None;
            }
            let token = u16::from_le_bytes([chunk[at], chunk[at + 1]]);
            at += 2;
            while (length - 1) >> distance_bits != 0 {
                distance_bits += 1;
            }
            let count = usize::from(token & (0xFFFF >> distance_bits)) + MIN_MATCH;
            let distance = usize::from(token >> (16 - distance_bits)) + 1;
            if length + count > CHUNK || distance > length {
                return None;
            }
            for index in length..length + count {
                out[index] = out[index - distance];
            }
            length += count;
        }
    }
    Some(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn sevenzip_lznt1dec_port(
        dest: &mut [u8],
        out_limit: usize,
        dest_len: usize,
        src: &[u8],
    ) -> usize {
        let mut dest_size = 0usize;
        let mut s = 0usize;
        let mut src_len = src.len();
        while dest_size < dest_len {
            if src_len < 2 || (dest_size & 0xFFF) != 0 {
                break;
            }
            let v = u32::from(src[s]) | u32::from(src[s + 1]) << 8;
            if v == 0 {
                break;
            }
            s += 2;
            src_len -= 2;
            let mut compr = (v & 0xFFF) as usize + 1;
            if compr > src_len {
                break;
            }
            src_len -= compr;
            if v & 0x8000 == 0 {
                if compr != 1 << 12 {
                    break;
                }
                dest[dest_size..dest_size + compr].copy_from_slice(&src[s..s + compr]);
                s += compr;
                dest_size += compr;
                continue;
            }
            if dest_size + (1 << 12) > out_limit || src[s] & 1 != 0 {
                return 0;
            }
            let mut bits = 4u32;
            let mut sb = 0usize;
            let mut pos = 0usize;
            loop {
                compr -= 1;
                let mut mask = u32::from(src[s + pos]) | 0x100;
                pos += 1;
                while mask > 1 && compr > 0 {
                    if mask & 1 == 0 {
                        if sb >= 1 << 12 {
                            return 0;
                        }
                        dest[dest_size] = src[s + pos];
                        dest_size += 1;
                        pos += 1;
                        sb += 1;
                        compr -= 1;
                    } else {
                        if compr < 2 {
                            return 0;
                        }
                        let v = u32::from(src[s + pos]) | u32::from(src[s + pos + 1]) << 8;
                        pos += 2;
                        compr -= 2;
                        while ((sb as u32).wrapping_sub(1) >> bits) != 0 {
                            bits += 1;
                        }
                        let len = (v & (0xFFFF >> bits)) as usize + 3;
                        if sb + len > 1 << 12 {
                            return 0;
                        }
                        let dist = (v >> (16 - bits)) as usize;
                        if dist >= sb {
                            return 0;
                        }
                        for _ in 0..len {
                            dest[dest_size] = dest[dest_size - dist - 1];
                            dest_size += 1;
                        }
                        sb += len;
                    }
                    mask >>= 1;
                }
                if compr == 0 {
                    break;
                }
            }
            s += pos;
        }
        dest_size
    }

    fn greedy_compress_every_token_form(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in data.chunks(CHUNK) {
            let packed = compress_chunk(chunk);
            if packed.len() >= CHUNK {
                out.extend_from_slice(&((CHUNK - 1) as u16 | 0x3000).to_le_bytes());
                out.extend_from_slice(chunk);
                out.resize(out.len() + CHUNK - chunk.len(), 0);
            } else {
                out.extend_from_slice(&((packed.len() - 1) as u16 | 0xB000).to_le_bytes());
                out.extend_from_slice(&packed);
            }
        }
        out
    }

    fn compress_chunk(chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < chunk.len() {
            let flags_at = out.len();
            out.push(0u8);
            for bit in 0..8 {
                if at == chunk.len() {
                    break;
                }
                let mut bits = 4;
                while (at.max(1) - 1) >> bits != 0 {
                    bits += 1;
                }
                let longest = (0xFFFF >> bits) + 3;
                let mut best = (0, 0);
                if at > 0 {
                    for distance in 1..=at.min(1 << bits) {
                        let mut count = 0;
                        while at + count < chunk.len()
                            && count < longest
                            && chunk[at + count] == chunk[at + count - distance]
                        {
                            count += 1;
                        }
                        if count > best.0 {
                            best = (count, distance);
                        }
                    }
                }
                if best.0 >= 3 {
                    let token = ((best.1 - 1) << (16 - bits)) | (best.0 - 3);
                    out.extend_from_slice(&(token as u16).to_le_bytes());
                    out[flags_at] |= 1 << bit;
                    at += best.0;
                } else {
                    out.push(chunk[at]);
                    at += 1;
                }
            }
        }
        out
    }

    fn sample(random: &mut Random, length: usize) -> Vec<u8> {
        let alphabet = 1 + random.next() % 255;
        let runs = random.next() % 4;
        (0..length)
            .map(|index| {
                if runs > 0 && index % 97 < 40 {
                    b'a'
                } else {
                    (random.next() % alphabet) as u8
                }
            })
            .collect()
    }

    #[test]
    fn compressed_units_round_trip_in_every_length() {
        let mut random = Random(0x1234_5678);
        for round in 0..300 {
            let length = match round {
                0 => 0,
                1 => 1,
                2 => CHUNK,
                3 => CHUNK + 1,
                _ => (random.next() % (4 * CHUNK as u64)) as usize,
            };
            let data = sample(&mut random, length);
            let packed = greedy_compress_every_token_form(&data);
            let mut unit = vec![0u8; 16 * CHUNK];
            let written = decode_whole_chunks(&mut unit, data.len(), &packed);
            assert!(written >= data.len(), "round {round}");
            assert_eq!(&unit[..data.len()], &data[..], "round {round}");
        }
    }

    #[test]
    fn every_damaged_stream_decodes_as_the_7zip_routine_does() {
        let mut random = Random(0x0BAD_F00D);
        for round in 0..1500 {
            let length = (random.next() % (3 * CHUNK as u64)) as usize;
            let data = sample(&mut random, length);
            let mut packed = greedy_compress_every_token_form(&data);
            match round % 4 {
                0 => {}
                1 if !packed.is_empty() => {
                    let at = (random.next() % packed.len() as u64) as usize;
                    packed[at] ^= 1 << (random.next() % 8);
                }
                2 => packed.truncate((random.next() % (packed.len() as u64 + 1)) as usize),
                _ => {
                    for _ in 0..1 + random.next() % 6 {
                        if !packed.is_empty() {
                            let at = (random.next() % packed.len() as u64) as usize;
                            packed[at] = random.next() as u8;
                        }
                    }
                }
            }
            let wanted = match random.next() % 3 {
                0 => data.len(),
                1 => (random.next() % (4 * CHUNK as u64)) as usize,
                _ => 16 * CHUNK,
            };
            let mut ours = vec![0u8; 16 * CHUNK];
            let mut theirs = vec![0u8; 16 * CHUNK];
            let got = decode_whole_chunks(&mut ours, wanted, &packed);
            let expected = sevenzip_lznt1dec_port(&mut theirs, 16 * CHUNK, wanted, &packed);
            assert_eq!(got, expected, "round {round}");
            assert_eq!(ours[..got], theirs[..expected], "round {round}");
        }
    }

    #[test]
    fn every_two_token_chunk_decodes_as_the_7zip_routine_does() {
        let mut ours = vec![0u8; CHUNK];
        let mut theirs = vec![0u8; CHUNK];
        for flags in 0..=255u8 {
            for first in [0u8, 1, 0x41] {
                for token in (0..=u16::MAX).step_by(4093) {
                    let body = [flags & !1, first, token as u8, (token >> 8) as u8, 0x42];
                    for size in 1..=body.len() {
                        let header = (size as u16 - 1) | 0xB000;
                        let mut stream = header.to_le_bytes().to_vec();
                        stream.extend_from_slice(&body[..size]);
                        ours.fill(0);
                        theirs.fill(0);
                        let got = decode_whole_chunks(&mut ours, CHUNK, &stream);
                        let expected = sevenzip_lznt1dec_port(&mut theirs, CHUNK, CHUNK, &stream);
                        assert_eq!(got, expected, "{stream:02x?}");
                        assert_eq!(ours, theirs, "{stream:02x?}");
                    }
                }
            }
        }
    }
}
