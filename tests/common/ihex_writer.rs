#![allow(dead_code)]
pub const DATA: u8 = 0;
pub const END: u8 = 1;
pub const SEGMENT: u8 = 2;
pub const START_SEGMENT: u8 = 3;
pub const LINEAR: u8 = 4;
pub const START_LINEAR: u8 = 5;
pub const END_RECORD: &str = ":00000001FF";

pub fn record(kind: u8, address: u16, data: &[u8]) -> String {
    record_with_count(kind, address, data, data.len() as u8)
}

pub fn record_with_count(kind: u8, address: u16, data: &[u8], count: u8) -> String {
    let mut fields = vec![count, (address >> 8) as u8, address as u8, kind];
    fields.extend_from_slice(data);
    let sum = fields.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    fields.push(sum.wrapping_neg());
    let digits: String = fields.iter().map(|byte| format!("{byte:02X}")).collect();
    format!(":{digits}")
}

pub fn segment(value: u16) -> String {
    record(SEGMENT, 0, &value.to_be_bytes())
}

pub fn linear(value: u16) -> String {
    record(LINEAR, 0, &value.to_be_bytes())
}

pub fn lines(records: &[String], newline: &str) -> Vec<u8> {
    records
        .iter()
        .flat_map(|record| [record.as_str(), newline])
        .collect::<String>()
        .into_bytes()
}

pub fn file(records: &[String]) -> Vec<u8> {
    let mut all = records.to_vec();
    all.push(END_RECORD.to_owned());
    lines(&all, "\n")
}

pub fn noise(length: usize, seed: u32) -> Vec<u8> {
    let mut state = seed | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

pub struct Random(u64);

impl Random {
    pub fn new(seed: u64) -> Self {
        Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    pub fn bytes(&mut self, length: usize) -> Vec<u8> {
        (0..length).map(|_| self.next() as u8).collect()
    }
}

fn breaks(random: &mut Random) -> String {
    match random.below(100) {
        0..60 => "\n".to_owned(),
        60..75 => "\r\n".to_owned(),
        75..80 => String::new(),
        80..95 => (0..1 + random.below(18))
            .map(|_| if random.chance(50) { '\r' } else { '\n' })
            .collect(),
        _ => ["\0", " ", "\t", "x", "\n\0"][random.below(5) as usize].to_owned(),
    }
}

pub fn random_file(random: &mut Random) -> Vec<u8> {
    let mut records = Vec::new();
    let mut address: u16 = [0, 0x100, 0xFFF0, random.below(65536) as u16][random.below(4) as usize];
    for _ in 0..1 + random.below(9) {
        let roll = random.below(100);
        if roll < 65 {
            let length = [1, 2, 16, 32, 1 + random.below(255) as usize][random.below(5) as usize];
            if random.chance(25) {
                address = random.below(65536) as u16;
            }
            let mut text = record(DATA, address, &random.bytes(length));
            if random.chance(10) {
                text = text.to_ascii_lowercase();
            }
            records.push(text);
            address = address.wrapping_add(length as u16);
        } else if roll < 80 {
            let kind = [SEGMENT, LINEAR][random.below(2) as usize];
            records.push(record(kind, 0, &random.bytes(2)));
        } else if roll < 90 {
            let kind = [START_SEGMENT, START_LINEAR][random.below(2) as usize];
            records.push(record(kind, 0, &random.bytes(4)));
        } else {
            let kind = [0, 1, 2, 3, 4, 5, 6, 0xFF][random.below(8) as usize];
            let address = [0, 1, 0x1234][random.below(3) as usize];
            let length = [0, 1, 2, 4, 5][random.below(5) as usize];
            records.push(record(kind, address, &random.bytes(length)));
        }
    }
    if random.chance(90) {
        records.push(END_RECORD.to_owned());
    }
    let mut bytes: Vec<u8> = records
        .iter()
        .flat_map(|record| format!("{record}{}", breaks(random)).into_bytes())
        .collect();
    let roll = random.below(100);
    if roll < 15 && !bytes.is_empty() {
        let at = random.below(bytes.len() as u64) as usize;
        let replacements = b"0123456789ABCDEFG:\r\n\0 ";
        bytes[at] = replacements[random.below(replacements.len() as u64) as usize];
    } else if roll < 25 && !bytes.is_empty() {
        bytes.truncate(random.below(bytes.len() as u64) as usize);
    }
    bytes
}
