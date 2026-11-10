# M14. Encrypted archives

Zipper extracts encrypted archives when given the password, and never writes a byte decrypted with a wrong one.

Scope:

- A password option on the command line, a prompt when no password is given, and a password field in the library options.
- ZipCrypto and WinZip AES in ZIP. ZipCrypto is read only for compatibility, and the docs say it is weak.
- 7z AES-256, including archives whose header is encrypted.
- A wrong password fails before publish, from the password check or the CRC, and the error says the password is wrong.
- The password stays out of errors, logs and progress reports, and its memory is cleared after use.

Exit checks:

1. A corpus of encrypted archives from 7-Zip, WinZip and Info-ZIP extracts the same trees as 7-Zip.
2. A test for each format shows that a wrong password changes nothing in the destination.
3. An exhaustive test fails decryption at every point it can fail, for each format, and finds the destination unchanged and no staging folder left each time.
4. The adversarial review covers the key derivation and the authentication checks.

Done so far, by 8 November 2026:

- ZIP ZipCrypto and WinZip AES decrypt in process, with `--password`, the prompt on Unix and the library option, as the [ZIP reader reference](../reference/zip-reader.md) describes. WinZip itself does not run on macOS, so the WinZip AES archives in `tests/data/encrypted-zip` come from 7-Zip instead. Info-ZIP writes only ZipCrypto, so it adds ZipCrypto archives alone.
- 7z AES-256 decrypts in process, in every folder layout 7-Zip writes and in encrypted headers, as [Encryption](../reference/7z-reader.md#encryption) describes. The default engine no longer sends an encrypted 7z to 7-Zip. A wrong password fails through the header CRC or the data CRC, before publish. A key derivation power above 7-Zip's limit of 24 is refused before any work. `tests/encrypted_7z.rs` checks 26 archives from `7zz` against the trees 7-Zip gave, a wrong password and none on each, and every point where decryption can fail. A Kani proof covers the property parser under `[7z-aes-properties]`.
- M14 closed on 8 November 2026. WinZip does not run on macOS and Info-ZIP writes only ZipCrypto, so the WinZip AES archives of exit check 1 come from 7-Zip, which writes the same format.
