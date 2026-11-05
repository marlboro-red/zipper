use crate::{BadField, Field, field_check};
use ctr::cipher::{KeyIvInit, StreamCipher};
use hmac::{KeyInit, Mac};
use zeroize::{Zeroize, Zeroizing};

pub(crate) const AES_METHOD: u16 = 99;
pub(crate) const AES_CODE: usize = 10;
const ZIP_CRYPTO_HEADER: usize = 12;
const AES_VERIFIER: usize = 2;
const AES_VENDOR: &[u8; 2] = b"AE";
const AES_FIELD: usize = 7;
// SPEC(WinZip AES, key generation): PBKDF2 with HMAC-SHA1 and 1000 iterations.
const AES_ITERATIONS: u32 = 1000;
const LONGEST_AES_KEYS: usize = 2 * 32 + AES_VERIFIER;
pub(crate) const LONGEST_HEADER: usize = 16 + AES_VERIFIER;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AesVersion {
    Ae1,
    Ae2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Strength {
    Aes128,
    Aes192,
    Aes256,
}

impl Strength {
    fn key_len(self) -> usize {
        match self {
            Strength::Aes128 => 16,
            Strength::Aes192 => 24,
            Strength::Aes256 => 32,
        }
    }

    fn salt_len(self) -> usize {
        self.key_len() / 2
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AesField {
    pub(crate) version: AesVersion,
    pub(crate) strength: Strength,
    pub(crate) method: u16,
}

// SPEC(WinZip AES, extra field 0x9901): vendor version, "AE", strength, then the real method.
pub(crate) fn aes_field(body: &[u8]) -> Field<AesField> {
    field_check(
        body.len() == AES_FIELD,
        "ZIP AES extra field is not 7 bytes",
    )?;
    let version = match u16::from_le_bytes([body[0], body[1]]) {
        1 => AesVersion::Ae1,
        2 => AesVersion::Ae2,
        _ => return Err(BadField("ZIP AES vendor version is not 1 or 2")),
    };
    field_check(
        body[2..4] == *AES_VENDOR,
        "ZIP AES extra field names another vendor",
    )?;
    let strength = match body[4] {
        1 => Strength::Aes128,
        2 => Strength::Aes192,
        3 => Strength::Aes256,
        _ => return Err(BadField("ZIP AES strength is not 1, 2 or 3")),
    };
    Ok(AesField {
        version,
        strength,
        method: u16::from_le_bytes([body[5], body[6]]),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AesExtra {
    Absent,
    Present(AesField),
    Malformed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Encryption {
    ZipCrypto,
    Aes(AesField),
}

impl Encryption {
    pub(crate) fn header_len(self) -> usize {
        match self {
            Encryption::ZipCrypto => ZIP_CRYPTO_HEADER,
            Encryption::Aes(field) => field.strength.salt_len() + AES_VERIFIER,
        }
    }

    fn trailer_len(self) -> usize {
        match self {
            Encryption::ZipCrypto => 0,
            Encryption::Aes(_) => AES_CODE,
        }
    }

    pub(crate) fn packed(self, compressed: u64) -> Field<u64> {
        compressed
            .checked_sub((self.header_len() + self.trailer_len()) as u64)
            .ok_or(BadField(
                "encrypted ZIP entry is too short for its encryption header",
            ))
    }

    // SPEC(WinZip AES, AE-2): the CRC field is zero and the authentication code replaces it.
    pub(crate) fn checks_crc(self) -> bool {
        !matches!(
            self,
            Encryption::Aes(AesField {
                version: AesVersion::Ae2,
                ..
            })
        )
    }
}

pub(crate) struct WrongPassword;

pub(crate) struct Decryptor(Cipher);

enum Cipher {
    ZipCrypto(Keys),
    Aes(Box<AesCipher>),
}

struct AesCipher {
    stream: AesStream,
    code: Code,
}

type Code = hmac::Hmac<sha1::Sha1>;

enum AesStream {
    Aes128(ctr::Ctr128LE<aes::Aes128>),
    Aes192(ctr::Ctr128LE<aes::Aes192>),
    Aes256(ctr::Ctr128LE<aes::Aes256>),
}

impl AesStream {
    fn new(strength: Strength, key: &[u8]) -> Self {
        let counter = 1u128.to_le_bytes();
        let iv = &counter[..];
        let length = "the key length matches the strength";
        match strength {
            Strength::Aes128 => Self::Aes128(KeyIvInit::new_from_slices(key, iv).expect(length)),
            Strength::Aes192 => Self::Aes192(KeyIvInit::new_from_slices(key, iv).expect(length)),
            Strength::Aes256 => Self::Aes256(KeyIvInit::new_from_slices(key, iv).expect(length)),
        }
    }

    fn apply(&mut self, data: &mut [u8]) {
        match self {
            Self::Aes128(stream) => stream.apply_keystream(data),
            Self::Aes192(stream) => stream.apply_keystream(data),
            Self::Aes256(stream) => stream.apply_keystream(data),
        }
    }
}

impl Decryptor {
    pub(crate) fn start(
        encryption: Encryption,
        password: &[u8],
        header: &[u8],
        zip_crypto_check: u8,
    ) -> Result<Self, WrongPassword> {
        assert_eq!(header.len(), encryption.header_len());
        match encryption {
            Encryption::ZipCrypto => {
                let mut keys = Keys::new(password);
                let mut plain = Zeroizing::new([0; ZIP_CRYPTO_HEADER]);
                plain.copy_from_slice(header);
                keys.decrypt(&mut plain[..]);
                // SPEC(APPNOTE 6.1.5): the last header byte is a check byte for the password.
                if plain[ZIP_CRYPTO_HEADER - 1] != zip_crypto_check {
                    return Err(WrongPassword);
                }
                Ok(Self(Cipher::ZipCrypto(keys)))
            }
            Encryption::Aes(field) => {
                let (salt, verifier) = header.split_at(field.strength.salt_len());
                let key_len = field.strength.key_len();
                let mut derived = Zeroizing::new([0; LONGEST_AES_KEYS]);
                let derived = &mut derived[..2 * key_len + AES_VERIFIER];
                pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password, salt, AES_ITERATIONS, derived);
                // SPEC(WinZip AES, key generation): the AES key, the HMAC key, then the verifier.
                let (keys, derived_verifier) = derived.split_at(2 * key_len);
                if derived_verifier != verifier {
                    return Err(WrongPassword);
                }
                let (aes_key, code_key) = keys.split_at(key_len);
                Ok(Self(Cipher::Aes(Box::new(AesCipher {
                    stream: AesStream::new(field.strength, aes_key),
                    code: <Code as KeyInit>::new_from_slice(code_key)
                        .expect("HMAC takes a key of any length"),
                }))))
            }
        }
    }

    pub(crate) fn decrypt(&mut self, data: &mut [u8]) {
        match &mut self.0 {
            Cipher::ZipCrypto(keys) => keys.decrypt(data),
            Cipher::Aes(aes) => {
                // SPEC(WinZip AES, authentication): the code covers the encrypted bytes.
                aes.code.update(data);
                aes.stream.apply(data);
            }
        }
    }

    pub(crate) fn authenticates(self, stored: &[u8; AES_CODE]) -> bool {
        match self.0 {
            Cipher::ZipCrypto(_) => true,
            Cipher::Aes(aes) => aes.code.verify_truncated_left(stored).is_ok(),
        }
    }
}

type KeyState = [u32; 3];

const ZIP_CRYPTO_KEYS: KeyState = [0x1234_5678, 0x2345_6789, 0x3456_7890];
const ZIP_CRYPTO_MULTIPLIER: u32 = 134_775_813;

struct Keys(KeyState);

// SPEC(APPNOTE 6.1.4): the keys start fixed and take in the password, then each plain byte.
impl Keys {
    fn new(password: &[u8]) -> Self {
        let mut keys = Self(ZIP_CRYPTO_KEYS);
        for &byte in password {
            keys.update(byte);
        }
        keys
    }

    fn update(&mut self, plain: u8) {
        let [first, second, third] = &mut self.0;
        *first = crc32_byte(*first, plain);
        *second = second
            .wrapping_add(*first & 0xff)
            .wrapping_mul(ZIP_CRYPTO_MULTIPLIER)
            .wrapping_add(1);
        *third = crc32_byte(*third, (*second >> 24) as u8);
    }

    fn keystream_byte(&self) -> u8 {
        let temp = (self.0[2] | 2) as u16;
        (temp.wrapping_mul(temp ^ 1) >> 8) as u8
    }

    fn decrypt(&mut self, data: &mut [u8]) {
        for byte in data {
            let plain = *byte ^ self.keystream_byte();
            self.update(plain);
            *byte = plain;
        }
    }
}

impl Drop for Keys {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

fn crc32_byte(crc: u32, byte: u8) -> u32 {
    (crc >> 8) ^ CRC32_TABLE[usize::from(crc as u8 ^ byte)]
}

type Crc32Table = [u32; 256];

const CRC32_TABLE: Crc32Table = {
    const REFLECTED_CRC32_POLYNOMIAL: u32 = 0xEDB8_8320;
    let mut table = [0; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ (REFLECTED_CRC32_POLYNOMIAL & (crc & 1).wrapping_neg());
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
};

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_aes_field(body: &[u8]) -> Option<AesField> {
        if body.len() != 7 || &body[2..4] != b"AE" {
            return None;
        }
        let version = match (body[0], body[1]) {
            (1, 0) => AesVersion::Ae1,
            (2, 0) => AesVersion::Ae2,
            _ => return None,
        };
        let strength = [Strength::Aes128, Strength::Aes192, Strength::Aes256]
            .get(usize::from(body[4]).checked_sub(1)?)
            .copied()?;
        Some(AesField {
            version,
            strength,
            method: u16::from(body[5]) | u16::from(body[6]) << 8,
        })
    }

    #[test]
    fn aes_fields_are_read_as_the_reference_reads_every_one_byte_change() {
        let valid = [2, 0, b'A', b'E', 3, 8, 0];
        for length in 0..=9 {
            let mut body = valid.to_vec();
            body.resize(length, 0);
            assert_eq!(
                aes_field(&body).ok(),
                reference_aes_field(&body),
                "{body:?}"
            );
            for at in 0..length {
                for value in 0..=u8::MAX {
                    let mut changed = body.clone();
                    changed[at] = value;
                    assert_eq!(
                        aes_field(&changed).ok(),
                        reference_aes_field(&changed),
                        "{changed:?}"
                    );
                }
            }
        }
    }

    fn reference_crc32(crc: u32, byte: u8) -> u32 {
        let mut crc = crc ^ u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
        crc
    }

    fn reference_zip_crypto_encrypt(password: &[u8], plain: &[u8]) -> Vec<u8> {
        let mut keys = [0x1234_5678u32, 0x2345_6789, 0x3456_7890];
        let update = |keys: &mut [u32; 3], byte: u8| {
            keys[0] = reference_crc32(keys[0], byte);
            keys[1] = keys[1]
                .wrapping_add(keys[0] & 0xff)
                .wrapping_mul(134_775_813)
                .wrapping_add(1);
            keys[2] = reference_crc32(keys[2], (keys[1] >> 24) as u8);
        };
        for &byte in password {
            update(&mut keys, byte);
        }
        plain
            .iter()
            .map(|&byte| {
                let temp = (keys[2] | 2) & 0xffff;
                let cipher = byte ^ ((temp * (temp ^ 1)) >> 8) as u8;
                update(&mut keys, byte);
                cipher
            })
            .collect()
    }

    #[test]
    fn crc_steps_match_crc32fast_on_every_byte() {
        for byte in 0..=u8::MAX {
            for crc in [0, 1, 0x1234_5678, u32::MAX] {
                assert_eq!(crc32_byte(crc, byte), reference_crc32(crc, byte));
            }
            let mut hasher = crc32fast::Hasher::new();
            hasher.update(&[byte]);
            assert_eq!(!crc32_byte(!0, byte), hasher.finalize());
        }
    }

    #[test]
    fn zip_crypto_decrypts_as_the_byte_at_a_time_reference_at_every_split() {
        let plain: Vec<u8> = (0..60u8).map(|at| at.wrapping_mul(37) ^ 0x5a).collect();
        for password in [&b""[..], b"zipper", &[0xff; 70]] {
            let cipher = reference_zip_crypto_encrypt(password, &plain);
            let check = plain[ZIP_CRYPTO_HEADER - 1];
            for first in 0..=plain.len() - ZIP_CRYPTO_HEADER {
                for second in first..=plain.len() - ZIP_CRYPTO_HEADER {
                    let header = &cipher[..ZIP_CRYPTO_HEADER];
                    let Ok(mut decryptor) =
                        Decryptor::start(Encryption::ZipCrypto, password, header, check)
                    else {
                        panic!("the right password failed its check byte");
                    };
                    let mut data = cipher[ZIP_CRYPTO_HEADER..].to_vec();
                    let (start, rest) = data.split_at_mut(first);
                    let (middle, end) = rest.split_at_mut(second - first);
                    for piece in [start, middle, end] {
                        decryptor.decrypt(piece);
                    }
                    assert_eq!(data, plain[ZIP_CRYPTO_HEADER..]);
                    assert!(decryptor.authenticates(&[0; AES_CODE]));
                }
            }
            let header = &cipher[..ZIP_CRYPTO_HEADER];
            for wrong in (0..=u8::MAX).filter(|&byte| byte != check) {
                assert!(Decryptor::start(Encryption::ZipCrypto, password, header, wrong).is_err());
            }
        }
    }

    const STRENGTHS: [Strength; 3] = [Strength::Aes128, Strength::Aes192, Strength::Aes256];

    fn aes(strength: Strength) -> Encryption {
        Encryption::Aes(AesField {
            version: AesVersion::Ae2,
            strength,
            method: 0,
        })
    }

    fn aes_header(strength: Strength, password: &[u8]) -> Vec<u8> {
        let mut header: Vec<u8> = (0..strength.salt_len() as u8).collect();
        let mut derived = vec![0; 2 * strength.key_len() + AES_VERIFIER];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password, &header, AES_ITERATIONS, &mut derived);
        header.extend_from_slice(&derived[2 * strength.key_len()..]);
        header
    }

    fn aes_decrypt_in_pieces(strength: Strength, data: &[u8], cuts: &[usize]) -> (Vec<u8>, bool) {
        let header = aes_header(strength, b"zipper");
        let Ok(mut decryptor) = Decryptor::start(aes(strength), b"zipper", &header, 0) else {
            panic!("the right password failed its verifier");
        };
        let mut plain = data.to_vec();
        let mut start = 0;
        for &cut in cuts.iter().chain([&data.len()]) {
            decryptor.decrypt(&mut plain[start..cut]);
            start = cut;
        }
        let whole = {
            let mut code = <Code as KeyInit>::new_from_slice(&{
                let mut derived = vec![0; 2 * strength.key_len() + AES_VERIFIER];
                pbkdf2::pbkdf2_hmac::<sha1::Sha1>(
                    b"zipper",
                    &header[..strength.salt_len()],
                    AES_ITERATIONS,
                    &mut derived,
                );
                derived[strength.key_len()..2 * strength.key_len()].to_vec()
            })
            .unwrap();
            code.update(data);
            code.finalize().into_bytes()
        };
        let stored: [u8; AES_CODE] = whole[..AES_CODE].try_into().unwrap();
        (plain, decryptor.authenticates(&stored))
    }

    #[test]
    fn aes_decrypts_and_authenticates_the_same_at_every_split() {
        let data: Vec<u8> = (0..70u8).map(|at| at.wrapping_mul(91)).collect();
        for strength in STRENGTHS {
            let (whole, authentic) = aes_decrypt_in_pieces(strength, &data, &[]);
            assert!(authentic);
            for first in 0..=data.len() {
                for second in (first..=data.len()).step_by(7) {
                    let (pieces, authentic) =
                        aes_decrypt_in_pieces(strength, &data, &[first, second]);
                    assert_eq!(pieces, whole, "{strength:?} cut at {first} and {second}");
                    assert!(authentic);
                }
            }
        }
    }

    #[test]
    fn aes_refuses_a_wrong_verifier_and_a_wrong_code() {
        for strength in STRENGTHS {
            let header = aes_header(strength, b"zipper");
            assert!(Decryptor::start(aes(strength), b"Zipper", &header, 0).is_err());
            for at in strength.salt_len()..header.len() {
                let mut wrong = header.clone();
                wrong[at] ^= 1;
                assert!(Decryptor::start(aes(strength), b"zipper", &wrong, 0).is_err());
            }
            let data = b"some encrypted bytes";
            let Ok(decryptor) = Decryptor::start(aes(strength), b"zipper", &header, 0) else {
                panic!("the right password failed its verifier");
            };
            assert!(!decryptor.authenticates(&[0; AES_CODE]));
            let (_, authentic) = aes_decrypt_in_pieces(strength, data, &[]);
            assert!(authentic);
            for at in 0..AES_CODE {
                let mut decryptor = Decryptor::start(aes(strength), b"zipper", &header, 0)
                    .ok()
                    .unwrap();
                let mut plain = data.to_vec();
                decryptor.decrypt(&mut plain);
                let mut code = [0; AES_CODE];
                let derived = {
                    let mut derived = vec![0; 2 * strength.key_len() + AES_VERIFIER];
                    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(
                        b"zipper",
                        &header[..strength.salt_len()],
                        AES_ITERATIONS,
                        &mut derived,
                    );
                    derived
                };
                let mut reference = <Code as KeyInit>::new_from_slice(
                    &derived[strength.key_len()..2 * strength.key_len()],
                )
                .unwrap();
                reference.update(data);
                code.copy_from_slice(&reference.finalize().into_bytes()[..AES_CODE]);
                code[at] ^= 0x10;
                assert!(
                    !decryptor.authenticates(&code),
                    "{strength:?} code byte {at}"
                );
            }
        }
    }

    fn field(id: u16, body: &[u8]) -> Vec<u8> {
        let mut field = id.to_le_bytes().to_vec();
        field.extend_from_slice(&(body.len() as u16).to_le_bytes());
        field.extend_from_slice(body);
        field
    }

    #[test]
    fn an_extra_area_is_aes_only_with_exactly_one_well_formed_aes_field() {
        let good = [1, 0, b'A', b'E', 1, 8, 0];
        let present = AesExtra::Present(reference_aes_field(&good).unwrap());
        let pieces: [(Vec<u8>, usize, bool); 5] = [
            (field(0x9901, &good), 1, true),
            (field(0x9901, &good[..6]), 1, false),
            (field(0x9901, &[2, 0, b'A', b'E', 4, 8, 0]), 1, false),
            (field(0x5455, &[1, 2, 3, 4, 5]), 0, true),
            (field(0x0001, &[]), 0, true),
        ];
        for first in 0..=pieces.len() {
            for second in 0..=pieces.len() {
                let mut extra = Vec::new();
                let (mut aes, mut well_formed) = (0, true);
                for &choice in &[first, second] {
                    if let Some((bytes, count, good)) = pieces.get(choice) {
                        extra.extend_from_slice(bytes);
                        aes += count;
                        well_formed &= *good || *count == 0;
                    }
                }
                let expected = match (aes, well_formed) {
                    (0, _) => AesExtra::Absent,
                    (1, true) => present,
                    _ => AesExtra::Malformed,
                };
                assert_eq!(
                    crate::zip_directory::aes_extra(&extra),
                    expected,
                    "{extra:?}"
                );
                for cut in 0..extra.len() {
                    let found = crate::zip_directory::aes_extra(&extra[..cut]);
                    assert!(found != present || extra[..cut].len() >= 11, "{extra:?}");
                }
            }
        }
    }

    #[test]
    fn pbkdf2_hmac_sha1_matches_rfc_6070() {
        let vectors: [(&[u8], &[u8], u32, &str); 4] = [
            (
                b"password",
                b"salt",
                1,
                "0c60c80f961f0e71f3a9b524af6012062fe037a6",
            ),
            (
                b"password",
                b"salt",
                2,
                "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957",
            ),
            (
                b"password",
                b"salt",
                4096,
                "4b007901b765489abead49d926f721d065a429c1",
            ),
            (
                b"passwordPASSWORDpassword",
                b"saltSALTsaltSALTsaltSALTsaltSALTsalt",
                4096,
                "3d2eec4fe41c849b80c8d83662c0e44a8b291a964cf2f07038",
            ),
        ];
        for (password, salt, rounds, expected) in vectors {
            let mut derived = vec![0; expected.len() / 2];
            pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password, salt, rounds, &mut derived);
            let hex: String = derived.iter().map(|byte| format!("{byte:02x}")).collect();
            assert_eq!(hex, expected);
        }
    }

    #[test]
    fn aes_ctr_starts_its_little_endian_counter_at_one() {
        use aes::cipher::{BlockCipherEncrypt, KeyInit as _};
        let key = [7u8; 16];
        let mut stream = AesStream::new(Strength::Aes128, &key);
        let mut keystream = [0u8; 48];
        stream.apply(&mut keystream);
        let block_cipher = aes::Aes128::new_from_slice(&key).unwrap();
        for (counter, block) in keystream.chunks(16).enumerate() {
            let mut expected = (counter as u128 + 1).to_le_bytes().into();
            block_cipher.encrypt_block(&mut expected);
            assert_eq!(block, &expected[..], "block {counter}");
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn aes_fields_never_panic_and_keep_only_documented_values() {
        let body: [u8; 9] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= body.len());
        if let Ok(field) = aes_field(&body[..length]) {
            assert!(length == AES_FIELD);
            assert!(body[2..4] == *AES_VENDOR && body[1] == 0);
            assert!(matches!(
                (body[0], field.version),
                (1, AesVersion::Ae1) | (2, AesVersion::Ae2)
            ));
            assert!(matches!(
                (body[4], field.strength),
                (1, Strength::Aes128) | (2, Strength::Aes192) | (3, Strength::Aes256)
            ));
            assert!(field.method == u16::from_le_bytes([body[5], body[6]]));
        }
    }

    #[kani::proof]
    fn the_payload_and_the_encryption_overhead_add_up_to_the_entry() {
        let compressed: u64 = kani::any();
        let encryption = match kani::any::<u8>() % 4 {
            0 => Encryption::ZipCrypto,
            strength => Encryption::Aes(AesField {
                version: if kani::any() {
                    AesVersion::Ae1
                } else {
                    AesVersion::Ae2
                },
                strength: [Strength::Aes128, Strength::Aes192, Strength::Aes256]
                    [usize::from(strength) - 1],
                method: kani::any(),
            }),
        };
        let header = encryption.header_len();
        assert!(header <= LONGEST_HEADER);
        let overhead = (header + encryption.trailer_len()) as u64;
        match encryption.packed(compressed) {
            Ok(packed) => assert!(packed + overhead == compressed),
            Err(_) => assert!(compressed < overhead),
        }
        if let Encryption::Aes(field) = encryption {
            assert!(2 * field.strength.key_len() + AES_VERIFIER <= LONGEST_AES_KEYS);
            assert!(encryption.checks_crc() == (field.version == AesVersion::Ae1));
        }
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn aes_extras_never_panic_and_find_a_whole_field() {
        let extra: [u8; 20] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= extra.len());
        if let AesExtra::Present(_) = crate::zip_directory::aes_extra(&extra[..length]) {
            assert!(length >= 4 + AES_FIELD);
        }
    }

    #[kani::proof]
    fn zip_crypto_key_steps_never_panic() {
        let mut keys = Keys(kani::any());
        let byte: u8 = kani::any();
        let _ = keys.keystream_byte();
        keys.update(byte);
    }
}
