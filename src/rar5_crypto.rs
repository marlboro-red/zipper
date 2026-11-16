use aes::cipher::{BlockCipherDecrypt, KeyInit};
use hmac::Mac;
use sha2::Digest;
use std::{
    collections::HashMap,
    io::{self, Read},
    rc::Rc,
};
use zeroize::Zeroizing;

pub(crate) const BLOCK: usize = 16;
pub(crate) const SALT_BYTES: usize = 16;
pub(crate) const IV_BYTES: usize = 16;
pub(crate) const CHECK_BYTES: usize = 12;
const CHECK_VALUE_BYTES: usize = 8;
const KEY_BYTES: usize = 32;
const CRC_BYTES: usize = 4;
pub(crate) const MOST_KDF_COUNT: u8 = 24;
const MOST_DERIVATIONS_AT_THE_CAP: u64 = 2;
const MOST_DERIVATIONS: usize = 1024;
// EXTERNAL(RAR 7.23, black box): the hash key is the PBKDF2 value 16 rounds past the key, and
// the password check value folds the value 32 rounds past it to 8 bytes.
const HASH_KEY_ROUNDS: u64 = 16;
const CHECK_ROUNDS: u64 = 32;
// EXTERNAL(RAR 7.23, black box): rar and unrar keep only the first 127 characters of a password.
const LONGEST_PASSWORD: usize = 127;
const AES_BUFFER: usize = 64 * 1024;

type HmacSha256 = hmac::Hmac<sha2::Sha256>;

fn rounds(count: u8) -> u64 {
    (1u64 << count) + CHECK_ROUNDS
}

const KDF_BUDGET: u64 = MOST_DERIVATIONS_AT_THE_CAP * ((1u64 << MOST_KDF_COUNT) + CHECK_ROUNDS);

pub(crate) struct Derived {
    key: Zeroizing<[u8; KEY_BYTES]>,
    hash_key: Zeroizing<[u8; KEY_BYTES]>,
    check: [u8; CHECK_VALUE_BYTES],
}

impl std::fmt::Debug for Derived {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("Derived")
    }
}

fn folded<const WIDTH: usize>(bytes: &[u8]) -> [u8; WIDTH] {
    let mut out = [0u8; WIDTH];
    for (at, byte) in bytes.iter().enumerate() {
        out[at % WIDTH] ^= byte;
    }
    out
}

// SPEC(RAR 5 technote, KDF count): PBKDF2 with 2^count iterations.
// EXTERNAL(RAR 7.23, black box): HMAC-SHA256 of the UTF-8 password and the salt, one 32-byte block.
#[inline(never)]
fn derive(password: &[u8], salt: &[u8; SALT_BYTES], count: u8) -> Derived {
    let keyed = <HmacSha256 as KeyInit>::new_from_slice(password).expect("HMAC takes any key");
    let mut first = keyed.clone();
    first.update(salt);
    first.update(&1u32.to_be_bytes());
    let mut round = Zeroizing::new([0u8; KEY_BYTES]);
    round.copy_from_slice(&first.finalize().into_bytes());
    let mut sum = Zeroizing::new(*round);
    let mut derived = Derived {
        key: Zeroizing::new([0; KEY_BYTES]),
        hash_key: Zeroizing::new([0; KEY_BYTES]),
        check: [0; CHECK_VALUE_BYTES],
    };
    let key_rounds = 1u64 << count;
    for done in 1..=key_rounds + CHECK_ROUNDS {
        if done > 1 {
            let mut next = keyed.clone();
            next.update(&round[..]);
            round.copy_from_slice(&next.finalize().into_bytes());
            for (total, byte) in sum.iter_mut().zip(round.iter()) {
                *total ^= byte;
            }
        }
        if done == key_rounds {
            derived.key.copy_from_slice(&sum[..]);
        } else if done == key_rounds + HASH_KEY_ROUNDS {
            derived.hash_key.copy_from_slice(&sum[..]);
        }
    }
    derived.check = folded(&sum[..]);
    derived
}

pub(crate) enum CheckValue {
    Matches,
    WrongPassword,
    Damaged,
}

impl Derived {
    // SPEC(RAR 5 technote, check value): 8 bytes from more PBKDF2 rounds, then a 4-byte checksum.
    // EXTERNAL(RAR 7.23, black box): the checksum is the first 4 bytes of SHA-256 of the 8.
    pub(crate) fn check(&self, value: &[u8; CHECK_BYTES]) -> CheckValue {
        let (password, checksum) = value.split_at(CHECK_VALUE_BYTES);
        if sha2::Sha256::digest(password)[..CHECK_BYTES - CHECK_VALUE_BYTES] != *checksum {
            CheckValue::Damaged
        } else if password == self.check {
            CheckValue::Matches
        } else {
            CheckValue::WrongPassword
        }
    }

    fn code(&self, data: &[u8]) -> [u8; KEY_BYTES] {
        let mut code = <HmacSha256 as KeyInit>::new_from_slice(&self.hash_key[..])
            .expect("HMAC takes any key");
        code.update(data);
        code.finalize().into_bytes().into()
    }

    // SPEC(RAR 5 technote, file encryption record): flag 0x0002 makes the checksums depend on the key.
    // EXTERNAL(RAR 7.23, black box): a CRC-32 becomes HMAC-SHA256 of its 4 little-endian bytes,
    // folded to 4 bytes, and a BLAKE2sp hash becomes HMAC-SHA256 of its 32 bytes.
    pub(crate) fn tweaked_crc(&self, crc: u32) -> u32 {
        u32::from_le_bytes(folded::<CRC_BYTES>(&self.code(&crc.to_le_bytes())))
    }

    pub(crate) fn tweaked_hash(&self, hash: &[u8; KEY_BYTES]) -> [u8; KEY_BYTES] {
        self.code(hash)
    }

    pub(crate) fn cipher(&self) -> aes::Aes256 {
        aes::Aes256::new_from_slice(&self.key[..]).expect("the key is 32 bytes")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KeyError {
    Needed,
    NotUtf8,
    OverBudget,
}

fn kept_password(password: &[u8]) -> Result<Zeroizing<Vec<u8>>, KeyError> {
    let text = std::str::from_utf8(password).map_err(|_| KeyError::NotUtf8)?;
    let end = text
        .char_indices()
        .nth(LONGEST_PASSWORD)
        .map_or(text.len(), |(at, _)| at);
    Ok(Zeroizing::new(text.as_bytes()[..end].to_vec()))
}

pub(crate) struct Keys {
    password: Option<Zeroizing<Vec<u8>>>,
    derived: HashMap<(u8, [u8; SALT_BYTES]), Rc<Derived>>,
    spent: u64,
}

impl Keys {
    pub(crate) fn new(password: Option<&[u8]>) -> Self {
        Keys {
            password: password.map(|password| Zeroizing::new(password.to_vec())),
            derived: HashMap::new(),
            spent: 0,
        }
    }

    fn charge(&mut self, count: u8) -> Result<(), KeyError> {
        let cost = rounds(count);
        if count > MOST_KDF_COUNT
            || self.spent + cost > KDF_BUDGET
            || self.derived.len() >= MOST_DERIVATIONS
        {
            return Err(KeyError::OverBudget);
        }
        self.spent += cost;
        Ok(())
    }

    pub(crate) fn derived(
        &mut self,
        count: u8,
        salt: &[u8; SALT_BYTES],
    ) -> Result<Rc<Derived>, KeyError> {
        if let Some(derived) = self.derived.get(&(count, *salt)) {
            return Ok(Rc::clone(derived));
        }
        let password = kept_password(self.password.as_ref().ok_or(KeyError::Needed)?)?;
        self.charge(count)?;
        let derived = Rc::new(derive(&password, salt, count));
        crate::scrub::scrub_stack();
        self.derived.insert((count, *salt), Rc::clone(&derived));
        Ok(derived)
    }
}

pub(crate) fn decrypt_cbc(cipher: &aes::Aes256, chain: &mut [u8; BLOCK], data: &mut [u8]) {
    for bytes in data.chunks_exact_mut(BLOCK) {
        let encrypted: [u8; BLOCK] = (&*bytes).try_into().expect("a whole block");
        let mut block = aes::Block::from(encrypted);
        cipher.decrypt_block(&mut block);
        for ((byte, plain), previous) in bytes.iter_mut().zip(block.iter()).zip(chain.iter()) {
            *byte = plain ^ previous;
        }
        *chain = encrypted;
    }
}

pub(crate) struct CbcReader<R> {
    inner: R,
    cipher: aes::Aes256,
    chain: [u8; BLOCK],
    buffer: Box<[u8]>,
    start: usize,
    plain: usize,
    end: usize,
}

impl<R: Read> CbcReader<R> {
    pub(crate) fn new(inner: R, cipher: aes::Aes256, iv: [u8; IV_BYTES]) -> Self {
        CbcReader {
            inner,
            cipher,
            chain: iv,
            buffer: vec![0; AES_BUFFER].into_boxed_slice(),
            start: 0,
            plain: 0,
            end: 0,
        }
    }
}

impl<R: Read> Read for CbcReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        while self.start == self.plain {
            self.buffer.copy_within(self.plain..self.end, 0);
            self.end -= self.plain;
            self.start = 0;
            self.plain = 0;
            let read = self.inner.read(&mut self.buffer[self.end..])?;
            self.end += read;
            let whole = self.end - self.end % BLOCK;
            decrypt_cbc(&self.cipher, &mut self.chain, &mut self.buffer[..whole]);
            self.plain = whole;
            if read == 0 && whole == 0 {
                if self.end == 0 {
                    return Ok(0);
                }
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "RAR 5 encrypted data ends inside an AES block",
                ));
            }
        }
        let count = output.len().min(self.plain - self.start);
        output[..count].copy_from_slice(&self.buffer[self.start..self.start + count]);
        self.start += count;
        Ok(count)
    }
}

#[cfg(all(test, feature = "zip-encryption"))]
mod oracle_tests {
    use super::*;

    fn pbkdf2(password: &[u8], salt: &[u8], rounds: u32) -> [u8; KEY_BYTES] {
        let mut out = [0u8; KEY_BYTES];
        pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password, salt, rounds, &mut out);
        out
    }

    #[test]
    fn the_key_hash_key_and_check_value_are_pbkdf2_at_their_rounds() {
        let salt: [u8; SALT_BYTES] = std::array::from_fn(|at| at as u8 * 3);
        for count in [0u8, 1, 2, 5, 10] {
            let derived = derive(b"zipper", &salt, count);
            let rounds = 1u32 << count;
            assert_eq!(*derived.key, pbkdf2(b"zipper", &salt, rounds), "{count}");
            assert_eq!(*derived.hash_key, pbkdf2(b"zipper", &salt, rounds + 16));
            let checked = pbkdf2(b"zipper", &salt, rounds + 32);
            assert_eq!(derived.check, folded::<8>(&checked));
        }
    }

    #[test]
    fn pbkdf2_hmac_sha256_matches_rfc_7914() {
        let derived = derive(b"passwd", b"salt\0\0\0\0\0\0\0\0\0\0\0\0", 0);
        assert_eq!(
            *derived.key,
            pbkdf2(b"passwd", b"salt\0\0\0\0\0\0\0\0\0\0\0\0", 1)
        );
        let mut rfc = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<sha2::Sha256>(b"passwd", b"salt", 1, &mut rfc);
        assert_eq!(
            rfc[..16],
            [
                0x55, 0xac, 0x04, 0x6e, 0x56, 0xe3, 0x08, 0x9f, 0xec, 0x16, 0x91, 0xc2, 0x25, 0x44,
                0xb6, 0x05
            ]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockCipherEncrypt;

    #[cfg(feature = "7z")]
    #[test]
    fn the_cap_is_the_one_the_7z_reader_keeps() {
        assert_eq!(MOST_KDF_COUNT, crate::sevenz_crypto::MOST_CYCLES_POWER);
    }

    #[test]
    fn a_password_keeps_its_first_127_characters() {
        let long: String = "日".repeat(130);
        let kept = kept_password(long.as_bytes()).unwrap();
        assert_eq!(&kept[..], "日".repeat(127).as_bytes());
        let short = "pässwörd";
        assert_eq!(
            &kept_password(short.as_bytes()).unwrap()[..],
            short.as_bytes()
        );
        assert_eq!(kept_password(b"\xFF").unwrap_err(), KeyError::NotUtf8);
    }

    #[test]
    fn the_budget_allows_two_derivations_at_the_cap_and_refuses_a_third() {
        let mut keys = Keys::new(Some(b"x"));
        assert!(keys.charge(MOST_KDF_COUNT).is_ok());
        assert!(keys.charge(MOST_KDF_COUNT).is_ok());
        assert_eq!(keys.charge(0), Err(KeyError::OverBudget));
        let mut keys = Keys::new(Some(b"x"));
        assert_eq!(keys.charge(MOST_KDF_COUNT + 1), Err(KeyError::OverBudget));
        let default = (KDF_BUDGET / rounds(15)) as usize;
        for _ in 0..default {
            assert!(keys.charge(15).is_ok());
        }
        assert_eq!(keys.charge(15), Err(KeyError::OverBudget));
    }

    #[test]
    fn cheap_derivations_are_capped_by_count_not_only_by_rounds() {
        let mut keys = Keys::new(Some(b"zipper"));
        for index in 0..MOST_DERIVATIONS {
            let salt: [u8; SALT_BYTES] = std::array::from_fn(|at| (index >> (8 * (at % 4))) as u8);
            keys.derived(0, &salt).unwrap();
        }
        assert_eq!(
            keys.derived(0, &[0xFF; SALT_BYTES]).unwrap_err(),
            KeyError::OverBudget
        );
        let known: [u8; SALT_BYTES] = std::array::from_fn(|at| (7 >> (8 * (at % 4))) as u8);
        assert!(keys.derived(0, &known).is_ok());
    }

    #[test]
    fn a_derived_key_is_reused_for_the_same_salt_and_count() {
        let mut keys = Keys::new(Some(b"zipper"));
        let salt = [1u8; SALT_BYTES];
        let first = keys.derived(3, &salt).unwrap();
        let again = keys.derived(3, &salt).unwrap();
        assert!(Rc::ptr_eq(&first, &again));
        assert_eq!(keys.spent, rounds(3));
        keys.derived(4, &salt).unwrap();
        keys.derived(3, &[2; SALT_BYTES]).unwrap();
        assert_eq!(keys.spent, 2 * rounds(3) + rounds(4));
        assert_eq!(
            Keys::new(None).derived(3, &salt).unwrap_err(),
            KeyError::Needed
        );
    }

    fn encrypted(key: &[u8; KEY_BYTES], iv: [u8; BLOCK], plain: &[u8]) -> Vec<u8> {
        let cipher = aes::Aes256::new_from_slice(key).unwrap();
        let mut chain = iv;
        let mut out = Vec::new();
        for piece in plain.chunks(BLOCK) {
            let mut block = aes::Block::default();
            for (at, byte) in block.iter_mut().enumerate() {
                *byte = piece.get(at).copied().unwrap_or(0) ^ chain[at];
            }
            cipher.encrypt_block(&mut block);
            chain.copy_from_slice(&block);
            out.extend_from_slice(&block);
        }
        out
    }

    struct Trickle<'a>(&'a [u8], usize);

    impl Read for Trickle<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let count = self.1.min(output.len()).min(self.0.len());
            output[..count].copy_from_slice(&self.0[..count]);
            self.0 = &self.0[count..];
            Ok(count)
        }
    }

    #[test]
    fn cbc_decrypts_as_the_block_at_a_time_encryption_under_every_read_split() {
        let key: [u8; KEY_BYTES] = std::array::from_fn(|at| at as u8 * 7);
        let iv: [u8; BLOCK] = std::array::from_fn(|at| 0xF0 ^ at as u8);
        let cipher = || aes::Aes256::new_from_slice(&key).unwrap();
        for blocks in [0, 1, 2, 15, 16, 17, 4097, 4100] {
            let plain: Vec<u8> = (0..blocks * BLOCK)
                .map(|at| (at * 31 % 251) as u8)
                .collect();
            let sealed = encrypted(&key, iv, &plain);
            for step in [1, 7, 16, 17, 1000, AES_BUFFER + 3] {
                let mut out = Vec::new();
                CbcReader::new(Trickle(&sealed, step), cipher(), iv)
                    .read_to_end(&mut out)
                    .unwrap();
                assert_eq!(out, plain, "{blocks} blocks read {step} at a time");
            }
            if !sealed.is_empty() {
                let mut out = Vec::new();
                let cut = &sealed[..sealed.len() - 1];
                let error = CbcReader::new(Trickle(cut, 5), cipher(), iv)
                    .read_to_end(&mut out)
                    .unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            }
        }
    }

    #[test]
    fn a_check_value_tells_a_wrong_password_from_a_damaged_value() {
        let derived = derive(b"zipper", &[9; SALT_BYTES], 2);
        let mut value = [0u8; CHECK_BYTES];
        value[..8].copy_from_slice(&derived.check);
        let tail = sha2::Sha256::digest(&value[..8]);
        value[8..].copy_from_slice(&tail[..4]);
        assert!(matches!(derived.check(&value), CheckValue::Matches));
        let mut damaged = value;
        damaged[11] ^= 1;
        assert!(matches!(derived.check(&damaged), CheckValue::Damaged));
        let other = derive(b"Zipper", &[9; SALT_BYTES], 2);
        assert!(matches!(other.check(&value), CheckValue::WrongPassword));
    }
}
