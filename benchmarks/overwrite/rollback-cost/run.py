"""Time `zipper extract --overwrite` into a full copy of each archive's tree.

Each run clones the source tree into a fresh folder under target/bench-work,
times the extraction alone, then checks the result with `diff -r` outside
the timed part. Binaries alternate order every round; round 0 is warmup."""
import json, os, resource, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = "/Users/aleph/Desktop/Projects/marlboro-red/zipper"
WORK = os.path.join(REPO, "target/bench-work/overwrite")
FIXTURES = {
    "flat-tiny": os.path.join(REPO, "target/bench-corpora/flat-tiny"),
    "dependencies": os.path.join(REPO, "target/bench-corpora/dependencies"),
}
BINARIES = ["before", "after"]
ROUNDS = int(sys.argv[1]) if len(sys.argv) > 1 else 10
FRESH = os.environ.get("MODE") == "fresh"


def busy():
    lines = subprocess.run(["ps", "-Ao", "pid,pcpu,comm"], capture_output=True, text=True).stdout.splitlines()[1:]
    found = []
    for line in lines:
        pid, cpu, command = line.split(None, 2)
        if float(cpu) >= 25 and int(pid) != os.getpid():
            found.append(f"{cpu} {command}")
    return found


def quiet(limit=60):
    deadline = time.monotonic() + limit
    while time.monotonic() < deadline:
        load = busy()
        if not load:
            return []
        time.sleep(1)
    return busy()


def run_once(binary, fixture):
    parent = tempfile.mkdtemp(dir=WORK)
    destination = os.path.join(parent, "out")
    name = fixture
    if not FRESH:
        os.mkdir(destination)
        subprocess.run(["cp", "-cR", os.path.join(FIXTURES[fixture], name), destination], check=True)
    load = quiet()
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    start = time.perf_counter()
    flags = [] if FRESH else ["--overwrite"]
    result = subprocess.run([os.path.join(HERE, binary), "extract", *flags,
                             os.path.join(HERE, f"{fixture}.zip"), "-o", destination],
                            capture_output=True, text=True)
    seconds = time.perf_counter() - start
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    if result.returncode != 0:
        sys.exit(f"{binary} {fixture}: {result.stderr}")
    same = subprocess.run(["diff", "-r", "-q", os.path.join(FIXTURES[fixture], name),
                           os.path.join(destination, name)], capture_output=True, text=True)
    if same.returncode != 0:
        sys.exit(f"{binary} {fixture}: output differs\n{same.stdout[:2000]}")
    extra = sorted(set(os.listdir(destination)) - {name})
    if extra:
        sys.exit(f"{binary} {fixture}: unexpected entries {extra}")
    shutil.rmtree(parent)
    cpu = (after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)
    return seconds, cpu, load


def main():
    os.makedirs(WORK, exist_ok=True)
    samples = []
    for round_number in range(ROUNDS):
        order = BINARIES if round_number % 2 == 0 else BINARIES[::-1]
        for fixture in FIXTURES:
            for binary in order:
                seconds, cpu, load = run_once(binary, fixture)
                samples.append(dict(round=round_number, warmup=round_number == 0, binary=binary,
                                    fixture=fixture, seconds=seconds, cpu_seconds=cpu, busy=load))
                print(round_number, fixture, binary, f"{seconds:.3f}", load, flush=True)
    json.dump(samples, open(os.path.join(HERE, "samples-fresh.json" if FRESH else "samples.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
