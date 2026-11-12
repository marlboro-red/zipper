"""Write the M29 fixture: tree/pkgNNN/subK/fileNNN.txt, 28 packages of 10 folders, 100 files each.

Usage: python3 make_archive.py REPO OUT_FOLDER
Writes OUT_FOLDER/tree (the source) and OUT_FOLDER/small-files.zip with extraction_matrix.write_zip.
"""
import random
import sys
from pathlib import Path

repo, out = Path(sys.argv[1]), Path(sys.argv[2])
sys.path.insert(0, str(repo / "scripts"))
from extraction_matrix import write_zip  # noqa: E402

rng = random.Random(27874)
words = ["alpha", "beta", "gamma", "delta", "import", "return", "value", "self", "def", "class",
         "if", "else", "for", "while", "None", "True", "list", "dict", "print", "len"]
lines = [" ".join(rng.choice(words) for _ in range(rng.randint(3, 12))) for _ in range(400)]
source = out / "tree"
for package in range(28):
    for sub in range(10):
        folder = source / f"pkg{package:03}" / f"sub{sub}"
        folder.mkdir(parents=True, exist_ok=True)
        for number in range(100):
            text = "\n".join(rng.choice(lines) for _ in range(rng.randint(5, 60))) + "\n"
            (folder / f"file{number:03}.txt").write_text(text)
write_zip(source, out / "small-files.zip")
