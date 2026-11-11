const IV: [u32; 8] = [
    0x6A09_E667,
    0xBB67_AE85,
    0x3C6E_F372,
    0xA54F_F53A,
    0x510E_527F,
    0x9B05_688C,
    0x1F83_D9AB,
    0x5BE0_CD19,
];

const SIGMA: [[usize; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];

const BLOCK: usize = 64;
const LEAVES: usize = 8;
const DIGEST: usize = 32;
const STRIPE: usize = BLOCK * LEAVES;
const FANOUT: u32 = LEAVES as u32;
const DEPTH: u32 = 2;

#[derive(Clone)]
struct Blake2s {
    state: [u32; 8],
    counter: u64,
    buffer: [u8; BLOCK],
    held: usize,
    last_node: bool,
}

fn mix(v: &mut [u32; 16], [a, b, c, d]: [usize; 4], x: u32, y: u32) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(12);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(8);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(7);
}

impl Blake2s {
    fn new(node_offset: u32, node_depth: u32, last_node: bool) -> Self {
        let mut state = IV;
        state[0] ^= DIGEST as u32 | (FANOUT << 16) | (DEPTH << 24);
        state[2] ^= node_offset;
        state[3] ^= (node_depth << 16) | ((DIGEST as u32) << 24);
        Blake2s {
            state,
            counter: 0,
            buffer: [0; BLOCK],
            held: 0,
            last_node,
        }
    }

    fn compress(&mut self, block: &[u8; BLOCK], last: bool) {
        let mut words = [0u32; 16];
        for (word, bytes) in words.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_le_bytes(bytes.try_into().unwrap());
        }
        let mut v = [0u32; 16];
        v[..8].copy_from_slice(&self.state);
        v[8..].copy_from_slice(&IV);
        v[12] ^= self.counter as u32;
        v[13] ^= (self.counter >> 32) as u32;
        if last {
            v[14] = !v[14];
            if self.last_node {
                v[15] = !v[15];
            }
        }
        for s in &SIGMA {
            mix(&mut v, [0, 4, 8, 12], words[s[0]], words[s[1]]);
            mix(&mut v, [1, 5, 9, 13], words[s[2]], words[s[3]]);
            mix(&mut v, [2, 6, 10, 14], words[s[4]], words[s[5]]);
            mix(&mut v, [3, 7, 11, 15], words[s[6]], words[s[7]]);
            mix(&mut v, [0, 5, 10, 15], words[s[8]], words[s[9]]);
            mix(&mut v, [1, 6, 11, 12], words[s[10]], words[s[11]]);
            mix(&mut v, [2, 7, 8, 13], words[s[12]], words[s[13]]);
            mix(&mut v, [3, 4, 9, 14], words[s[14]], words[s[15]]);
        }
        for (index, word) in self.state.iter_mut().enumerate() {
            *word ^= v[index] ^ v[index + 8];
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            if self.held == BLOCK {
                self.counter += BLOCK as u64;
                let block = self.buffer;
                self.compress(&block, false);
                self.held = 0;
            }
            let take = (BLOCK - self.held).min(data.len());
            self.buffer[self.held..self.held + take].copy_from_slice(&data[..take]);
            self.held += take;
            data = &data[take..];
        }
    }

    fn finish(mut self) -> [u8; DIGEST] {
        self.counter += self.held as u64;
        self.buffer[self.held..].fill(0);
        let block = self.buffer;
        self.compress(&block, true);
        let mut digest = [0u8; DIGEST];
        for (bytes, word) in digest.chunks_exact_mut(4).zip(self.state) {
            bytes.copy_from_slice(&word.to_le_bytes());
        }
        digest
    }
}

#[derive(Clone)]
pub(crate) struct Blake2sp {
    leaves: [Blake2s; LEAVES],
    position: u64,
}

impl Blake2sp {
    pub(crate) fn new() -> Self {
        Blake2sp {
            leaves: std::array::from_fn(|index| Blake2s::new(index as u32, 0, index == LEAVES - 1)),
            position: 0,
        }
    }

    pub(crate) fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let in_stripe = (self.position % STRIPE as u64) as usize;
            let leaf = in_stripe / BLOCK;
            let take = (BLOCK - in_stripe % BLOCK).min(data.len());
            self.leaves[leaf].update(&data[..take]);
            self.position += take as u64;
            data = &data[take..];
        }
    }

    pub(crate) fn finish(self) -> [u8; DIGEST] {
        let mut root = Blake2s::new(0, 1, true);
        for leaf in self.leaves {
            root.update(&leaf.finish());
        }
        root.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(data: &[u8], piece: usize) -> String {
        let mut hash = Blake2sp::new();
        for chunk in data.chunks(piece.max(1)) {
            hash.update(chunk);
        }
        hash.finish().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn sample(length: usize) -> Vec<u8> {
        (0..length)
            .map(|index| (index * 7 + index / 251) as u8)
            .collect()
    }

    const PYTHON_HASHLIB_DIGESTS: &[(usize, &str)] = &[
        (
            0,
            "dd0e891776933f43c7d032b08a917e25741f8aa9a12c12e1cac8801500f2ca4f",
        ),
        (
            1,
            "a6b9eecc25227ad788c99d3f236debc8da408849e9a5178978727a81457f7239",
        ),
        (
            63,
            "d2155a9b45debaa37d475e384d97c898aebe8cb52cd80e8368a56766e93a813a",
        ),
        (
            64,
            "9ba0c37f1ab9fdd4b656b42702049eec67f76c1ef76c3722b7b1e2a019674230",
        ),
        (
            65,
            "8180a53baa5bdb2cfd292e8b084ce88dbd6b2b61b60193732e392b11f46ac605",
        ),
        (
            511,
            "ff3f4303d794d7fbfc6a4fee0d289f1649cc315e2fd5c2fa9180ce913fee1e31",
        ),
        (
            512,
            "08a65a68f54f72077155adc2e339db1dec6c860f640cb47584cb2f2d8977a8c1",
        ),
        (
            513,
            "f405ed3422923e4cd0d141ab4ff130914fff2eb9e7f49588a5c02387e5d343ac",
        ),
        (
            5000,
            "b75dc4b976d4430da9c9489618f863d97ac1d2f37f0a6d78c5c20cb74a71b2f1",
        ),
    ];

    #[test]
    fn digests_match_scripts_blake2sp_oracle_py_for_every_split() {
        for &(length, expected) in PYTHON_HASHLIB_DIGESTS {
            let data = sample(length);
            for piece in [1, 3, 64, 65, 511, 512, 4096] {
                assert_eq!(digest(&data, piece), expected, "{length} in {piece}");
            }
        }
    }
}
