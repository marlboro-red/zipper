#!/usr/bin/env python3
"""Print BLAKE2sp digests of the sample inputs in src/blake2sp.rs.

hashlib's BLAKE2s takes the tree parameters, so BLAKE2sp is eight leaves
over interleaved 64-byte blocks and one root over their digests.
"""
import hashlib
import sys


def blake2sp(data: bytes) -> str:
    common = dict(digest_size=32, fanout=8, depth=2, leaf_size=0, inner_size=32)
    leaves = []
    for leaf in range(8):
        part = b"".join(
            data[start + leaf * 64 : start + leaf * 64 + 64]
            for start in range(0, len(data), 512)
        )
        leaves.append(
            hashlib.blake2s(
                part, node_offset=leaf, node_depth=0, last_node=leaf == 7, **common
            ).digest()
        )
    return hashlib.blake2s(
        b"".join(leaves), node_offset=0, node_depth=1, last_node=True, **common
    ).hexdigest()


def sample(length: int) -> bytes:
    return bytes((index * 7 + index // 251) & 0xFF for index in range(length))


if __name__ == "__main__":
    lengths = [int(arg) for arg in sys.argv[1:]] or [0, 1, 63, 64, 65, 511, 512, 513, 5000]
    for length in lengths:
        print(f"        ({length}, \"{blake2sp(sample(length))}\"),")
