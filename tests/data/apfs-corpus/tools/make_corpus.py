"""Write the APFS corpus with hdiutil, diskutil and ditto on macOS.

usage: python3 -I make_corpus.py SCRATCH OUTPUT

SCRATCH is an empty folder for images and mount points, and OUTPUT is the
folder that receives each image compressed with `xz -9`. Every image is
attached under SCRATCH with -nobrowse and detached before the next step.
"""
import os
import random
import shutil
import subprocess
import sys

SCRATCH, OUTPUT = sys.argv[1], sys.argv[2]
MOUNTS = os.path.join(SCRATCH, "mnt")


def run(*command):
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout


def attach(image):
    os.makedirs(MOUNTS, exist_ok=True)
    listing = run("hdiutil", "attach", "-nobrowse", "-mountroot", MOUNTS, image)
    rows = [line.split("\t") for line in listing.splitlines()]
    disks = [row[0].strip() for row in rows]
    mounted = [row[-1].strip() for row in rows if row[-1].strip().startswith(MOUNTS)]
    return disks[0], mounted


def detach(disk):
    run("hdiutil", "detach", disk)


def seeded(seed, length):
    generator = random.Random(seed)
    return bytes(generator.randrange(256) for _ in range(length))


def source_folder():
    source = os.path.join(SCRATCH, "compressed-source")
    os.makedirs(source, exist_ok=True)
    with open(os.path.join(source, "big.txt"), "w") as handle:
        handle.write("".join("line %d of a compressible text file\n" % n for n in range(9000)))
    with open(os.path.join(source, "medium.txt"), "w") as handle:
        handle.write("medium compressible text " * 200)
    with open(os.path.join(source, "noise.bin"), "wb") as handle:
        handle.write(seeded(7, 70000) + b"tail" * 4000)
    with open(os.path.join(source, "small.txt"), "w") as handle:
        handle.write("small\n")
    return source


def fill(root, source):
    with open(os.path.join(root, "hello.txt"), "w") as handle:
        handle.write("hello from APFS\n")
    with open(os.path.join(root, "random.bin"), "wb") as handle:
        handle.write(seeded(1, 5000))
    for name in ["café ünïcödé.txt", "日本語のファイル.txt", "emoji 😀.txt"]:
        with open(os.path.join(root, name), "w") as handle:
            handle.write(name + "\n")
    os.symlink("hello.txt", os.path.join(root, "link-to-hello"))
    os.makedirs(os.path.join(root, "links"))
    with open(os.path.join(root, "links", "first.txt"), "w") as handle:
        handle.write("three names, one file\n")
    os.link(os.path.join(root, "links", "first.txt"), os.path.join(root, "links", "second.txt"))
    os.link(os.path.join(root, "links", "first.txt"), os.path.join(root, "third.txt"))
    deep = root
    for level in range(40):
        deep = os.path.join(deep, "d%02d" % level)
    os.makedirs(deep)
    with open(os.path.join(deep, "bottom.txt"), "w") as handle:
        handle.write("forty folders down\n")
    wide = os.path.join(root, "wide")
    os.makedirs(wide)
    for number in range(300):
        with open(os.path.join(wide, "file%03d.txt" % number), "w") as handle:
            handle.write("file %d\n" % number)
    modes = os.path.join(root, "modes")
    os.makedirs(modes)
    for name, mode in [("run.sh", 0o755), ("read-only.txt", 0o444), ("private.txt", 0o600)]:
        path = os.path.join(modes, name)
        with open(path, "w") as handle:
            handle.write(name + "\n")
        os.chmod(path, mode)
    os.chmod(modes, 0o555)
    run("xattr", "-w", "user.small", "a small value", os.path.join(root, "hello.txt"))
    run("xattr", "-w", "user.large", "x" * 6000, os.path.join(root, "random.bin"))
    run("ditto", "--hfsCompression", source, os.path.join(root, "compressed"))


def special(root):
    with open(os.path.join(root, "original.txt"), "w") as handle:
        handle.write("cloned text\n" * 2000)
    run("cp", "-c", os.path.join(root, "original.txt"), os.path.join(root, "clone.txt"))
    with open(os.path.join(root, "sparse.bin"), "wb") as handle:
        handle.seek(1 << 20)
        handle.write(seeded(3, 8192))
        handle.truncate(3 << 20)
    os.makedirs(os.path.join(root, "up"))
    os.symlink("../original.txt", os.path.join(root, "up", "parent-link"))


def volume_image(name, kind, volume, populate, size="8m"):
    image = os.path.join(SCRATCH, name + ".dmg")
    run("hdiutil", "create", "-size", size, "-fs", kind, "-volname", volume, image)
    disk, mounted = attach(image)
    populate(mounted[0])
    detach(disk)
    return image


def store(image, name):
    target = os.path.join(OUTPUT, name)
    shutil.copyfile(image, target)
    run("xz", "-9", "-f", target)


def convert(image, form, name):
    output = os.path.join(SCRATCH, name)
    run("hdiutil", "convert", image, "-format", form, "-o", output)
    if form == "UDTO":
        output += ".cdr"
    store(output, name)


def main():
    os.makedirs(OUTPUT, exist_ok=True)
    source = source_folder()
    plain = volume_image("plain", "APFS", "Corpus", lambda root: fill(root, source))
    for form in ["UDZO", "UDBZ", "ULFO", "ULMO", "UDRO", "UDCO"]:
        convert(plain, form, "apfs-%s.dmg" % form.lower())
    convert(plain, "UDTO", "apfs-gpt.img")
    sensitive = volume_image("sensitive", "Case-sensitive APFS", "Sensitive",
                             lambda root: fill(root, source))
    for form in ["UDZO", "ULFO"]:
        convert(sensitive, form, "sensitive-%s.dmg" % form.lower())
    odd = volume_image("special", "APFS", "Special", special)
    convert(odd, "UDZO", "special-udzo.dmg")

    raw = os.path.join(SCRATCH, "raw.dmg")
    run("hdiutil", "create", "-size", "8m", "-fs", "APFS", "-layout", "NONE",
        "-volname", "Raw", raw)
    disk, mounted = attach(raw)
    fill(mounted[0], source)
    detach(disk)
    store(raw, "apfs-raw.img")

    volumes = os.path.join(SCRATCH, "volumes.sparseimage")
    run("hdiutil", "create", "-size", "1100m", "-type", "SPARSE", "-fs", "APFS",
        "-volname", "First", volumes)
    disk, mounted = attach(volumes)
    with open(os.path.join(mounted[0], "first.txt"), "w") as handle:
        handle.write("in the first volume\n")
    reference = None
    for line in run("diskutil", "info", mounted[0]).splitlines():
        if "APFS Container" in line:
            reference = line.split(":")[1].strip()
    run("diskutil", "apfs", "addVolume", reference, "Case-sensitive APFS", "Second", "-nomount")
    run("diskutil", "apfs", "addVolume", reference, "APFS", "Third", "-nomount")
    second = os.path.join(MOUNTS, "Second")
    os.makedirs(second, exist_ok=True)
    run("diskutil", "mount", "-mountPoint", second, reference + "s2")
    with open(os.path.join(second, "Name.txt"), "w") as handle:
        handle.write("upper case\n")
    with open(os.path.join(second, "other.txt"), "w") as handle:
        handle.write("in the second volume\n")
    run("diskutil", "unmount", reference + "s2")
    detach(disk)
    convert(volumes, "UDZO", "volumes-udzo.dmg")

    far = os.path.join(SCRATCH, "far.sparseimage")
    run("hdiutil", "create", "-size", "6g", "-type", "SPARSE", "-fs", "APFS",
        "-volname", "Far", far)
    disk, mounted = attach(far)
    filler = os.path.join(mounted[0], "filler.bin")
    with open(filler, "wb") as handle:
        chunk = b"\0" * (1 << 20)
        for _ in range(4400):
            handle.write(chunk)
    with open(os.path.join(mounted[0], "far.txt"), "w") as handle:
        handle.write("written past 4 GiB\n" * 100)
    os.remove(filler)
    detach(disk)
    convert(far, "UDZO", "far-6g-udzo.dmg")


main()
