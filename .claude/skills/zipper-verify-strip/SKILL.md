---
name: zipper-verify-strip
description: Check that stripping leading folders (`Strip::Prefix`, `Strip::Components`, `--strip-prefix`, `--strip-components`) places every entry where its strip says, refuses collisions and escaping links, and publishes nothing on failure. Use before committing any change to src/placement.rs or to where a reader renames its entries for a strip, and when a stripped extraction lands a file in the wrong place.
---

# Verify stripped extraction

A strip renames every entry before it is written. A wrong rename writes a file to the wrong path, merges two entries into one, or turns a safe link into one that leaves the destination. Three checks cover it.

## The checks

| Check | What it proves |
| --- | --- |
| `every_short_path_lands_where_its_strip_says` in src/placement.rs | On every path of up to 4 segments over `a`, `b` and `ab`, each count from 0 to 5 and each prefix gives the path that a plain split by `/` gives. A prefix match always rebuilds the full path. |
| `placed_names_fit_exactly_when_no_file_is_doubled_or_used_as_a_folder` | On every set of 3 entries over 2 segments, the collision check refuses exactly the sets with two files at one path or a file under a path another entry needs as a folder |
| `every_corpus_archive_strips_to_its_manifest_without_the_leading_folders` in tests/strip.rs | Every ZIP, tar and 7z corpus archive, on both ZIP engines, stripped by each of its folders and by 1 to 3 segments, gives the manifest's tree re-placed by path segments alone. Where that tree holds a collision or a link that leaves the destination, the extraction fails and publishes nothing. |

## Run them

```sh
cargo test --release --lib placement
cargo test --release --test strip
```

Then run `cargo xtask verify-sync strip`.

## Proof that the checks can fail

| Change | Result |
| --- | --- |
| A prefix also matches a longer name, so `a` takes `ab/c` | the placement unit test fails |
| `Components(n)` drops one segment too few | the corpus test fails |
| tar skips the link check at the stripped path | the corpus test fails: `dir-link -> ../text` leaves the destination after 2 segments |
| ZIP or 7z keeps the full name of a placed entry | the corpus test fails |
