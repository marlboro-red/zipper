use aes::cipher::{BlockCipherDecrypt, KeyInit};
use sha2::Digest;
use std::{io::Read, sync::Arc};
use zeroize::Zeroizing;

pub(crate) const AES_METHOD: [u8; 4] = [0x06, 0xF1, 0x07, 0x01];
pub(crate) const BLOCK: u64 = 16;
pub(crate) const AES_BUFFER: usize = 64 * 1024;
const LONGEST_SALT: usize = 16;
const IV_BYTES: usize = 16;
const KEY_BYTES: usize = 32;
const COUNTER_BYTES: usize = 8;
const DECRYPTED_AT_ONCE: usize = 16;
// EXTERNAL(7-Zip 7zAes.cpp): k_NumCyclesPower_Supported_MAX, which 7-Zip skips for one-byte properties.
pub(crate) const MOST_CYCLES_POWER: u8 = 24;
const POWER_MASK: u8 = 0x3F;
const SALT_EXTRA: u8 = 0x80;
const IV_EXTRA: u8 = 0x40;
// SPEC(7-Zip 7zAes.cpp CalcKey): power 0x3F takes the salt, then the password, as the key unhashed.
const UNHASHED_KEY: u8 = 0x3F;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Properties {
    pub(crate) cycles_power: u8,
    pub(crate) salt: [u8; LONGEST_SALT],
    pub(crate) salt_len: usize,
    pub(crate) iv: [u8; IV_BYTES],
}

impl Properties {
    fn salt(&self) -> &[u8] {
        &self.salt[..self.salt_len]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BadProperties {
    Malformed,
    TooManyCycles(u8),
}

fn stream_size(extra: bool, nibble: u8) -> usize {
    usize::from(extra) + usize::from(nibble)
}

pub(crate) fn properties(data: &[u8]) -> Result<Properties, BadProperties> {
    let mut found = Properties {
        cycles_power: 0,
        salt: [0; LONGEST_SALT],
        salt_len: 0,
        iv: [0; IV_BYTES],
    };
    let Some((&first, rest)) = data.split_first() else {
        return Ok(found);
    };
    found.cycles_power = first & POWER_MASK;
    if first & (SALT_EXTRA | IV_EXTRA) == 0 {
        if !rest.is_empty() {
            return Err(BadProperties::Malformed);
        }
    } else {
        let Some((&second, rest)) = rest.split_first() else {
            return Err(BadProperties::Malformed);
        };
        let salt_len = stream_size(first & SALT_EXTRA != 0, second >> 4);
        let iv_len = stream_size(first & IV_EXTRA != 0, second & 0x0F);
        if rest.len() != salt_len + iv_len {
            return Err(BadProperties::Malformed);
        }
        let (salt, iv) = rest.split_at(salt_len);
        found.salt[..salt_len].copy_from_slice(salt);
        found.salt_len = salt_len;
        found.iv[..iv_len].copy_from_slice(iv);
    }
    if found.cycles_power > MOST_CYCLES_POWER && found.cycles_power != UNHASHED_KEY {
        return Err(BadProperties::TooManyCycles(found.cycles_power));
    }
    Ok(found)
}

pub(crate) struct Key(Zeroizing<[u8; KEY_BYTES]>);

impl std::fmt::Debug for Key {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("Key")
    }
}

// SPEC(7-Zip 7zAes.cpp CalcKey): SHA-256 over 2^power rounds of the salt, the UTF-16LE password
// and a little-endian 64-bit round counter.
#[inline(never)]
fn derive_key(properties: &Properties, password: &[u8]) -> Key {
    let mut key = Zeroizing::new([0u8; KEY_BYTES]);
    if properties.cycles_power == UNHASHED_KEY {
        for (slot, &byte) in key.iter_mut().zip(properties.salt().iter().chain(password)) {
            *slot = byte;
        }
        return Key(key);
    }
    let mut round = Zeroizing::new(Vec::with_capacity(
        properties.salt_len + password.len() + COUNTER_BYTES,
    ));
    round.extend_from_slice(properties.salt());
    round.extend_from_slice(password);
    round.extend_from_slice(&[0; COUNTER_BYTES]);
    let counter_at = round.len() - COUNTER_BYTES;
    let mut hasher = sha2::Sha256::new();
    for _ in 0..1u64 << properties.cycles_power {
        hasher.update(&round[..]);
        for byte in &mut round[counter_at..] {
            *byte = byte.wrapping_add(1);
            if *byte != 0 {
                break;
            }
        }
    }
    key.copy_from_slice(&hasher.finalize());
    Key(key)
}

pub(crate) enum KeyError {
    Needed,
    NotUtf8,
}

pub(crate) struct Keys {
    password: Option<Zeroizing<Vec<u8>>>,
    derived: Vec<(u8, Vec<u8>, Arc<Key>)>,
}

impl Keys {
    pub(crate) fn new(password: Option<&[u8]>) -> Self {
        Keys {
            password: password.map(|password| Zeroizing::new(password.to_vec())),
            derived: Vec::new(),
        }
    }

    pub(crate) fn key(&mut self, properties: &Properties) -> Result<Arc<Key>, KeyError> {
        let salt = properties.salt();
        if let Some((_, _, key)) = self
            .derived
            .iter()
            .find(|(power, known, _)| *power == properties.cycles_power && known == salt)
        {
            return Ok(Arc::clone(key));
        }
        let password = self.password.as_ref().ok_or(KeyError::Needed)?;
        let text = std::str::from_utf8(password).map_err(|_| KeyError::NotUtf8)?;
        let mut utf16 = Zeroizing::new(Vec::with_capacity(2 * text.len()));
        for unit in text.encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        let key = Arc::new(derive_key(properties, &utf16));
        crate::zip_crypto::scrub_stack();
        self.derived
            .push((properties.cycles_power, salt.to_vec(), Arc::clone(&key)));
        Ok(key)
    }
}

pub(crate) struct CbcReader<R> {
    inner: R,
    cipher: aes::Aes256,
    chain: [u8; BLOCK as usize],
    buffer: Vec<u8>,
    start: usize,
    plain: usize,
    end: usize,
}

impl<R: Read> CbcReader<R> {
    pub(crate) fn new(inner: R, key: &Key, iv: [u8; IV_BYTES]) -> Self {
        CbcReader {
            inner,
            cipher: aes::Aes256::new_from_slice(&key.0[..]).expect("the key is 32 bytes"),
            chain: iv,
            buffer: vec![0; AES_BUFFER],
            start: 0,
            plain: 0,
            end: 0,
        }
    }

    fn decrypt(&mut self, whole: usize) {
        let mut blocks = [aes::Block::default(); DECRYPTED_AT_ONCE];
        let block = BLOCK as usize;
        for piece in self.buffer[..whole].chunks_mut(DECRYPTED_AT_ONCE * block) {
            let count = piece.len() / block;
            for (decrypted, encrypted) in blocks.iter_mut().zip(piece.chunks_exact(block)) {
                decrypted.copy_from_slice(encrypted);
            }
            self.cipher.decrypt_blocks(&mut blocks[..count]);
            for (decrypted, bytes) in blocks[..count].iter().zip(piece.chunks_exact_mut(block)) {
                let encrypted: [u8; BLOCK as usize] = bytes.try_into().expect("a whole block");
                for ((byte, plain), previous) in bytes.iter_mut().zip(decrypted).zip(&self.chain) {
                    *byte = plain ^ previous;
                }
                self.chain = encrypted;
            }
        }
    }
}

impl<R: Read> Read for CbcReader<R> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.start < self.plain {
                let count = output.len().min(self.plain - self.start);
                output[..count].copy_from_slice(&self.buffer[self.start..self.start + count]);
                self.start += count;
                return Ok(count);
            }
            self.buffer.copy_within(self.plain..self.end, 0);
            self.end -= self.plain;
            self.start = 0;
            self.plain = 0;
            let read = self.inner.read(&mut self.buffer[self.end..])?;
            if read == 0 {
                if self.end == 0 {
                    return Ok(0);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "7z encrypted stream ends inside an AES block",
                ));
            }
            self.end += read;
            let whole = self.end - self.end % BLOCK as usize;
            self.decrypt(whole);
            self.plain = whole;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockCipherEncrypt;

    #[derive(Debug, PartialEq, Eq)]
    enum SevenZip {
        Accepted(u8, Vec<u8>, [u8; 16]),
        InvalidArgument,
        NotImplemented,
    }

    fn seven_zip_properties(data: &[u8]) -> SevenZip {
        let mut iv = [0u8; 16];
        if data.is_empty() {
            return SevenZip::Accepted(0, Vec::new(), iv);
        }
        let b0 = data[0];
        let power = b0 & 0x3F;
        if b0 & 0xC0 == 0 {
            return if data.len() == 1 {
                SevenZip::Accepted(power, Vec::new(), iv)
            } else {
                SevenZip::InvalidArgument
            };
        }
        if data.len() <= 1 {
            return SevenZip::InvalidArgument;
        }
        let b1 = data[1];
        let salt_size = usize::from((b0 >> 7) & 1) + usize::from(b1 >> 4);
        let iv_size = usize::from((b0 >> 6) & 1) + usize::from(b1 & 0x0F);
        if data.len() != 2 + salt_size + iv_size {
            return SevenZip::InvalidArgument;
        }
        let salt = data[2..2 + salt_size].to_vec();
        iv[..iv_size].copy_from_slice(&data[2 + salt_size..]);
        if power <= 24 || power == 0x3F {
            SevenZip::Accepted(power, salt, iv)
        } else {
            SevenZip::NotImplemented
        }
    }

    #[test]
    fn properties_are_read_as_7zip_reads_them_for_every_two_leading_bytes_and_length() {
        let mut data: Vec<u8> = (0..34u8).map(|at| at.wrapping_mul(29) ^ 0xA5).collect();
        let mut refused_where_7zip_grinds = 0;
        for first in 0..=u8::MAX {
            for second in 0..=u8::MAX {
                data[0] = first;
                data[1] = second;
                for length in 0..=data.len() {
                    let ours = properties(&data[..length]);
                    match (seven_zip_properties(&data[..length]), ours) {
                        (SevenZip::Accepted(power, salt, iv), Ok(found)) => {
                            assert_eq!(
                                (found.cycles_power, found.salt(), found.iv),
                                (power, &salt[..], iv)
                            );
                        }
                        (
                            SevenZip::Accepted(power, ..),
                            Err(BadProperties::TooManyCycles(ours)),
                        ) => {
                            assert_eq!(power, ours);
                            assert!(first & 0xC0 == 0 && length == 1);
                            assert!(power > MOST_CYCLES_POWER && power != UNHASHED_KEY);
                            refused_where_7zip_grinds += 1;
                        }
                        (SevenZip::InvalidArgument, Err(BadProperties::Malformed)) => {}
                        (SevenZip::NotImplemented, Err(BadProperties::TooManyCycles(power))) => {
                            assert_eq!(power, first & 0x3F);
                        }
                        (theirs, ours) => {
                            panic!("{:?}: 7-Zip {theirs:?}, ours {ours:?}", &data[..length])
                        }
                    }
                }
            }
        }
        assert_eq!(refused_where_7zip_grinds, (0x3F - 25) * 256);
    }

    fn reference_key(power: u8, salt: &[u8], password: &[u8]) -> [u8; 32] {
        if power == 0x3F {
            let mut key = [0u8; 32];
            let joined: Vec<u8> = salt.iter().chain(password).copied().collect();
            let used = joined.len().min(32);
            key[..used].copy_from_slice(&joined[..used]);
            return key;
        }
        let mut hasher = sha2::Sha256::new();
        for round in 0..1u64 << power {
            hasher.update(salt);
            hasher.update(password);
            hasher.update(round.to_le_bytes());
        }
        hasher.finalize().into()
    }

    fn with(power: u8, salt: &[u8]) -> Properties {
        let mut found = Properties {
            cycles_power: power,
            salt: [0; 16],
            salt_len: salt.len(),
            iv: [0; 16],
        };
        found.salt[..salt.len()].copy_from_slice(salt);
        found
    }

    #[test]
    fn keys_match_the_reference_for_every_salt_length_and_small_power() {
        let password: Vec<u8> = "zïp😀".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let salt: Vec<u8> = (1..=16).collect();
        for length in 0..=16 {
            for power in (0..=10).chain([0x3F]) {
                let properties = with(power, &salt[..length]);
                for password in [&password[..], &[], &[7; 40]] {
                    assert_eq!(
                        *derive_key(&properties, password).0,
                        reference_key(power, &salt[..length], password),
                        "power {power}, salt {length}"
                    );
                }
            }
        }
        assert_eq!(
            *derive_key(&with(19, &[]), &password).0,
            reference_key(19, &[], &password)
        );
    }

    #[test]
    fn keys_are_derived_once_per_salt_and_power_and_need_a_utf8_password() {
        let mut keys = Keys::new(Some("zipper".as_bytes()));
        let first = keys.key(&with(4, b"ab")).ok().unwrap();
        let again = keys.key(&with(4, b"ab")).ok().unwrap();
        let other = keys.key(&with(4, b"ac")).ok().unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert!(!Arc::ptr_eq(&first, &other));
        assert_eq!(keys.derived.len(), 2);
        let utf16: Vec<u8> = "zipper".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(*first.0, reference_key(4, b"ab", &utf16));
        assert!(matches!(
            Keys::new(None).key(&with(4, b"")),
            Err(KeyError::Needed)
        ));
        assert!(matches!(
            Keys::new(Some(b"\xFF")).key(&with(4, b"")),
            Err(KeyError::NotUtf8)
        ));
    }

    fn reference_encrypt(key: &[u8; 32], iv: [u8; 16], plain: &[u8]) -> Vec<u8> {
        let cipher = aes::Aes256::new_from_slice(key).unwrap();
        let mut chain = iv;
        let mut encrypted = Vec::new();
        for block in plain.chunks(16) {
            let mut bytes = aes::Block::default();
            for (at, byte) in bytes.iter_mut().enumerate() {
                *byte = block.get(at).copied().unwrap_or(0) ^ chain[at];
            }
            cipher.encrypt_block(&mut bytes);
            chain.copy_from_slice(&bytes);
            encrypted.extend_from_slice(&bytes);
        }
        encrypted
    }

    struct Pieces {
        data: Vec<u8>,
        at: usize,
        sizes: Vec<usize>,
        turn: usize,
    }

    impl Read for Pieces {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let size = self.sizes[self.turn % self.sizes.len()];
            self.turn += 1;
            let count = size.min(buffer.len()).min(self.data.len() - self.at);
            buffer[..count].copy_from_slice(&self.data[self.at..self.at + count]);
            self.at += count;
            Ok(count)
        }
    }

    fn decrypt_in_reads(
        encrypted: &[u8],
        key: &Key,
        iv: [u8; 16],
        inner: &[usize],
        outer: usize,
    ) -> std::io::Result<Vec<u8>> {
        let pieces = Pieces {
            data: encrypted.to_vec(),
            at: 0,
            sizes: inner.to_vec(),
            turn: 0,
        };
        let mut reader = CbcReader::new(pieces, key, iv);
        let mut plain = Vec::new();
        let mut buffer = vec![0u8; outer];
        loop {
            match reader.read(&mut buffer)? {
                0 => return Ok(plain),
                read => plain.extend_from_slice(&buffer[..read]),
            }
        }
    }

    #[test]
    fn cbc_decrypts_as_the_block_at_a_time_reference_under_every_read_split() {
        let key = Key(Zeroizing::new(std::array::from_fn(|at| at as u8 * 7)));
        let iv: [u8; 16] = std::array::from_fn(|at| 0xF0 ^ at as u8);
        for blocks in [0, 1, 2, 15, 16, 17, 33] {
            let plain: Vec<u8> = (0..blocks * 16).map(|at| (at * 31 % 251) as u8).collect();
            let encrypted = reference_encrypt(&key.0, iv, &plain);
            for inner in [&[1][..], &[5, 16, 3], &[16], &[17, 100], &[1000]] {
                for outer in [1, 15, 16, 17, 64, 4096] {
                    let decrypted = decrypt_in_reads(&encrypted, &key, iv, inner, outer).unwrap();
                    assert_eq!(decrypted, plain, "{blocks} blocks, {inner:?}, {outer}");
                }
            }
        }
        let plain: Vec<u8> = (0..AES_BUFFER * 2 + 48)
            .map(|at| (at % 253) as u8)
            .collect();
        let encrypted = reference_encrypt(&key.0, iv, &plain);
        for (inner, outer) in [(&[AES_BUFFER + 9][..], 100_000), (&[4093, 7], 5000)] {
            assert_eq!(
                decrypt_in_reads(&encrypted, &key, iv, inner, outer).unwrap(),
                plain
            );
        }
    }

    #[test]
    fn a_stream_that_ends_inside_a_block_is_refused_at_every_length() {
        let key = Key(Zeroizing::new([3; 32]));
        let encrypted = reference_encrypt(&key.0, [0; 16], &[9; 64]);
        for length in 0..=encrypted.len() {
            let result = decrypt_in_reads(&encrypted[..length], &key, [0; 16], &[7], 16);
            assert_eq!(result.is_ok(), length % 16 == 0, "{length}");
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(17)]
    fn aes_properties_never_panic_and_keep_their_salt_iv_and_cycles() {
        let data: [u8; 2 + LONGEST_SALT + IV_BYTES] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= data.len());
        let result = properties(&data[..length]);
        if length == 0 {
            assert!(matches!(result, Ok(found) if found.cycles_power == 0
                && found.salt_len == 0
                && found.iv == [0; IV_BYTES]));
            return;
        }
        let power = data[0] & 0x3F;
        let refused_power = power > MOST_CYCLES_POWER && power != UNHASHED_KEY;
        let (salt_len, iv_len) = if data[0] & 0xC0 == 0 {
            (0, 0)
        } else {
            (
                usize::from(data[0] >> 7) + usize::from(data[1] >> 4),
                usize::from((data[0] >> 6) & 1) + usize::from(data[1] & 0x0F),
            )
        };
        let whole = if data[0] & 0xC0 == 0 {
            length == 1
        } else {
            length >= 2 && length == 2 + salt_len + iv_len
        };
        match result {
            Ok(found) => {
                assert!(whole && !refused_power);
                assert!(found.cycles_power == power);
                assert!(found.salt_len == salt_len && salt_len <= LONGEST_SALT);
                assert!(iv_len <= IV_BYTES);
                for at in 0..LONGEST_SALT {
                    let expected = if at < salt_len { data[2 + at] } else { 0 };
                    assert!(found.salt[at] == expected);
                }
                for at in 0..IV_BYTES {
                    let expected = if at < iv_len {
                        data[2 + salt_len + at]
                    } else {
                        0
                    };
                    assert!(found.iv[at] == expected);
                }
            }
            Err(BadProperties::TooManyCycles(found)) => {
                assert!(whole && refused_power && found == power);
            }
            Err(BadProperties::Malformed) => assert!(!whole),
        }
    }
}
