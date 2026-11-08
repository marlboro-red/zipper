# In-process FLV and SWF reader

Zipper reads Flash Video (FLV) files and SWF files with its own Rust reader, in the tree `7zz x` writes. The default engine sends a file here when it starts with `FLV` and the version byte 1, or with `FWS`, `CWS` or `ZWS` and a version byte below 64. A file that a ZIP, tar, cpio, `ar`, RPM, xar, CAB, LZH, ARJ, compound file, WIM, CHM, 7z, NTFS, disc or disk reader claims goes there first. `--zip-engine native` also reads these files in process, and `--zip-engine backend` hands them to 7-Zip.

## FLV files

An FLV file holds a 13-byte header and a run of tags. The reader opens a file only when 7-Zip opens it:

- The header sets no flag other than audio (4) and video (1), gives a header length of 9, and gives a first previous tag size of 0.
- Every tag is audio (8), video (9) or script data (18). It holds at least one byte of data and has a stream ID of 0, and the previous tag size after it equals its length.
- The file holds at least one audio or video tag, and at most 2^23 of them.

Script data, such as `onMetaData`, is not extracted. What the reader writes depends on how many streams the file holds:

- A file of one stream, audio or video, writes that stream's raw data. That is the data of every tag without its first byte, which names the codec, joined in order. Every tag must name the same codec, or the file is refused, as in 7-Zip.
- A file of audio and video writes one FLV file for each. Each starts with an FLV header that sets only its own stream's flag and copies that stream's tags whole, timestamps included. The codec may change from tag to tag.

The raw data takes the codec as its extension, from this table. A file of both streams writes `NAME.audio.flv` and `NAME.video.flv`.

| Code | Audio | Video |
| ---: | --- | --- |
| 0 | `pcm` | `video0` |
| 1 | `adpcm` | `jpeg` |
| 2 | `mp3` | `h263` |
| 3 | `pcm_le` | `screen` |
| 4 | `nellymoser16` | `vp6` |
| 5 | `nellymoser8` | `vp6alpha` |
| 6 | `nellymoser` | `screen2` |
| 7 | `g711a` | `avc` |
| 8 | `g711m` | `video8` |
| 9 | `audio9` | `video9` |
| 10 | `aac` | `video10` |
| 11 | `speex` | `video11` |
| 12 | `audio12` | `video12` |
| 13 | `audio13` | `video13` |
| 14 | `mp3` | `video14` |
| 15 | `audio15` | `video15` |

## SWF files

A SWF file starts with `FWS`, `CWS` or `ZWS`, a version byte and the length of the uncompressed file.

- An uncompressed SWF, `FWS`, writes each tag before its end tag as a file named `N.CODE`. `N` counts the tags from 0 and `CODE` is the tag code, so `3.21` is the fourth tag, a `DefineBitsJPEG2`. The file holds the tag's body. The end tag must end exactly at the length the header declares, as in 7-Zip.
- A SWF compressed with zlib, `CWS`, or with LZMA, `ZWS`, writes the uncompressed SWF it holds, with `FWS` in its header. The reader does not check that the result is a movie, as 7-Zip does not. The declared length must be at least 8 bytes, or 17 for LZMA, as 7-Zip requires.
- The zlib stream must decode to exactly the declared length, less the 8 header bytes, and end with a matching Adler-32. The LZMA stream takes its properties, dictionary and packed size from the 9 bytes after the header. It must decode to exactly the declared length and then end, with an end marker or a clean range coder.

## Output names

A compressed SWF and an FLV hold no names of their own, so the reader names their output after the archive, as 7-Zip does:

- The archive name loses its last extension. A name with no extension, or whose only dot comes first, such as `.flv`, keeps it whole.
- A compressed SWF adds `~.swf`, so `movie.swf` writes `movie~.swf` and `movie` writes `movie~.swf`.
- An FLV drops trailing spaces from that stem, then adds a dot and the extension, so `movie.flv` writes `movie.mp3`. A name with no extension gains `~` first, so `movie` writes `movie~.mp3`.
- `\` and `:` become `_`, and trailing spaces are dropped, with trailing dots too on Windows. 7-Zip on macOS and Linux keeps `:`.
- A name that equals the archive's own name, ignoring ASCII case, gains `~`, where 7-Zip would write over the archive in its own folder. `x.aac.aac` holding AAC writes `x.aac.aac~`.

A file split into [volumes](zip-reader.md#split-volumes) is named from its first volume's name without `.001`.

## Safety checks

- The output goes to a hidden staging folder and is published only when the whole file reads, as with every reader.
- Each output file is charged against free space, or `--max-extracted-bytes`, before it is written. A compressed SWF is charged its declared length before a byte is decoded.
- A zlib or LZMA stream is decoded within its declared length. A stream that would write one byte past it is refused at that byte, so a small file that declares a short length cannot expand without bound.
- An LZMA window, the dictionary or the declared length if smaller, larger than 256 MiB is refused with a hint to use `--zip-engine backend`.
- The FLV reader passes over the file once to check every tag and count the streams, then once for each output file. It holds one tag header at a time, so a long file costs no more memory than a short one. 7-Zip reads every tag into memory.
- The SWF reader writes each tag as it reads it, and keeps only the list of files it wrote. With `--path` or `--include`, it also keeps every tag's name, about 13 bytes each, to report a selected path that matches no tag. A SWF of 1,000,000 tags peaks at 62 MB under `zipper test`, where 7-Zip peaks at 75 MB.
- `--path`, `--include` and `--exclude` select output files by name. An FLV or a compressed SWF takes `--strip-components` and `--strip-prefix` as every reader does. The tags of an uncompressed SWF sit in no folder, so any strip leaves nothing and is refused.

## Differences from 7-Zip

Zipper refuses these files, which 7-Zip extracts:

- An FLV whose tags stop before the end of the file. 7-Zip stops at the first tag it cannot read, writes the tags before it and warns. That covers a file cut inside a tag and data after the last tag. It also covers a tag that holds no data, has a stream ID or is of another type, and a previous tag size that does not match.
- An uncompressed SWF with data after the length its header declares. 7-Zip warns.
- A zlib SWF with data after its zlib stream. 7-Zip extracts it without a warning.
- An LZMA SWF whose file goes on past its packed size. 7-Zip warns.
- An LZMA SWF whose stream decodes past its declared length, or does not end cleanly there. 7-Zip writes the declared length and stops.
- An LZMA SWF whose stream ends before its packed size.
- An LZMA SWF whose window is larger than 256 MiB.
- An uncompressed SWF of more than 1,000,000 tags, the limit the cpio, `ar` and CAB readers share. 7-Zip extracts 4,194,305 tags.

## Limits

| Limit | Value |
| --- | --- |
| FLV audio and video tags | at most 2^23, as in 7-Zip |
| SWF tags | at most 1,000,000 |
| LZMA window | at most 256 MiB |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

## Tests

`tests/data/swf-corpus` holds FLV and SWF files from ffmpeg, of every codec it writes into each, and SWF files compressed from two of them with zlib and LZMA. `make_corpus.py` in that folder writes them and records the tree `7zz x` writes from each in `trees.json`. ffmpeg's SWF of audio alone declares a length it never writes, so 7-Zip and Zipper both refuse it.

`tests/swf_corpus.rs` checks every corpus file against `trees.json`, and against 7-Zip where it is installed. It also writes FLV of every audio format and video codec, alone and together, with script data between. It writes SWF of every tag form and frame size. It compresses SWF with zlib at every level, with liblzma's LZMA and its end marker, and with 7-Zip's LZMA without one. Each extracts byte for byte as `7zz x` extracts it, names included.

`tests/hostile_swf.rs` cuts six small files at every length and flips a bit in each byte. Each result is refused when 7-Zip refuses it, and otherwise extracts as 7-Zip extracts it or is refused for a reason listed above. The full sweep, under `--ignored`, flips three bits in each byte and samples five corpus files too. Other tests cap the output below a declared length, decode a zlib stream of 64 MiB that declares less, and give LZMA a window of 1 GiB. Kani proofs under `[swf-fields]` in `verification/coverage.txt` cover the SWF and FLV header, tag and length readers.
