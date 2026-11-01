# Zipper

Zipper is a Rust archive engine that aims for fast compression and decompression at competitive compression ratios.
Native archives use the versioned `.zipper` format.
Zipper is an experimental foundation, not a performance leader yet.

## Build and try it

```sh
cargo build --release
target/release/zipper create backup.zipper ./data
target/release/zipper extract backup.zipper --output restored
target/release/zipper extract backup.zipper --output selected --path data/subdir
target/release/zipper list backup.zipper
target/release/zipper test backup.zipper
target/release/zipper --threads 4 create backup.zipper ./data --level 1
target/release/zipper --threads 4 create dense.zipper ./data --level 6 --block-size-kib 4096
```

Keep the archive outside the input tree.
Creation never overwrites an archive.
Extraction requires a new destination directory with an existing parent.

## Documentation

- [Usage](docs/usage.md) covers creating, extracting, selecting paths, ZIP engines, and tuning.
- [Design](docs/design.md) explains the extraction architecture and why it is built that way.
- [Research agenda](docs/research.md) lists open questions and frontier experiments.
- [Results](docs/results.md) gives current numbers per workload, each linked to its experiment record.
- [Native format reference](docs/reference/format.md) defines the `.zipper` format contract.
- [ZIP reader reference](docs/reference/zip-reader.md) lists supported ZIP features, limits, and memory budgets.
- [Duplicate-checking reference](docs/reference/duplicates.md) describes the `duplicates` command.
- [Windows release notes](docs/reference/windows.md) cover installing and running the Windows early release.
- [Experiment log](benchmarks/README.md) indexes every benchmark record and its reproduction steps.

## Limitations

Zipper archives regular files and directories with UTF-8 paths only.
It does not implement permissions, timestamps, links, extended attributes, or encryption.
V2 CRC32 checks payloads and decoded index metadata for accidental corruption, but archives are not authenticated.
V1 metadata remains unchecked.
Inputs must remain unchanged during creation.
Block-scoped solid packing has no cross-block history.
Extraction has no total expanded-size quota yet.
Use private destination parents, because publication does not defend against concurrent filesystem changes by other users.
The [native format reference](docs/reference/format.md) and the [ZIP reader reference](docs/reference/zip-reader.md) give the details.

## Development

Run `cargo xtask gate fast` before pushing, or run `cargo xtask install-hooks` once so pre-push runs it.
CI runs `cargo xtask gate full`.
[AGENTS.md](AGENTS.md) lists the rules for working in this repository.
