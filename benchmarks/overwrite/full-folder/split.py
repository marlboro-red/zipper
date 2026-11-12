"""Split a samply profile into stages by inclusive function name.

Usage: split.py PROFILE.json.gz SYMS.json [top N leaf functions]
For each thread: sample count (wall at the sampling interval) and CPU delta,
attributed to the first stage pattern found walking from root to leaf... we use
the innermost matching stage so nested stages split cleanly.
"""
import bisect, gzip, json, sys, collections

profile = json.load(gzip.open(sys.argv[1]))
syms = json.load(open(sys.argv[2]))
names = syms['string_table']
tables = {}
for lib in syms['data']:
    table = sorted((entry['rva'], entry['size'], names[entry['symbol']]) for entry in lib['symbol_table'])
    tables[lib['debug_name']] = ([row[0] for row in table], table)
libs = profile['libs']
interval = profile['meta']['interval']

STAGES = [
    ('staging cleanup', ['tempfile::dir::TempDir', 'remove_dir_all', 'drop_in_place::<zipper::staging::Staging>', 'remove_files_on_threads', 'remove_files_in']),
    ('plan', ['zipper::staging::plan']),
    ('link check', ['ensure_links_stay_in_archive_paths']),
    ('folder replacement', ['replace_folder', 'ensure_only_replaced_entries', 'staging::contents']), ('swaps', ['replace_by_swap']),
    ('carry_out: create', ['Staging>::create', 'move_into_place']),
    ('carry_out', ['Staging>::carry_out']),
    ('modes of created folders', ['apply_with_default']),
    ('publish_checking other', ['publish_checking']),
    ('read archive / write staging', ['native_zip']),
]


def resolve(thread, frame):
    func = thread['frameTable']['func'][frame]
    resource = thread['funcTable']['resource'][func]
    address = thread['frameTable']['address'][frame]
    if resource is None or resource < 0 or address is None or address < 0:
        return thread['stringArray'][thread['funcTable']['name'][func]]
    lib_index = thread['resourceTable']['lib'][resource]
    lib = libs[lib_index]['debugName']
    if lib not in tables:
        return lib
    starts, table = tables[lib]
    position = bisect.bisect_right(starts, address) - 1
    if position >= 0 and address < table[position][0] + max(table[position][1], 1 << 20):
        return table[position][2]
    return lib


overall = collections.Counter()
overall_cpu = collections.Counter()
for thread in profile['threads']:
    samples = thread['samples']
    stacks = thread['stackTable']
    cache = {}

    def frames(stack):
        if stack in cache:
            return cache[stack]
        out = []
        cursor = stack
        while cursor is not None:
            out.append(resolve(thread, stacks['frame'][cursor]))
            cursor = stacks['prefix'][cursor]
        cache[stack] = out
        return out

    by_stage = collections.Counter()
    cpu_by_stage = collections.Counter()
    leaves = collections.Counter()
    deltas = samples.get('threadCPUDelta') or [0] * samples['length']
    for stack, delta in zip(samples['stack'], deltas):
        if stack is None:
            continue
        names_here = frames(stack)
        stage = 'other'
        for label, patterns in STAGES:
            if any(p in name for name in names_here for p in patterns):
                stage = label
                break
        by_stage[stage] += 1
        cpu_by_stage[stage] += delta or 0
        if thread['isMainThread']:
            leaves[(stage, names_here[0], names_here[1] if len(names_here) > 1 else '')] += 1
    overall.update(by_stage)
    overall_cpu.update(cpu_by_stage)
    total = sum(by_stage.values())
    if total == 0:
        continue
    print(f"thread {thread['name']} main={thread['isMainThread']} samples={total} (~{total*interval:.0f} ms) cpu={sum(cpu_by_stage.values())/1000:.0f} ms")
    for stage, count in by_stage.most_common():
        print(f"  {stage:40} {count*interval:8.0f} ms  cpu {cpu_by_stage[stage]/1000:8.0f} ms")
    if thread['isMainThread'] and len(sys.argv) > 3:
        for (stage, leaf, caller), count in leaves.most_common(int(sys.argv[3])):
            print(f"    {count*interval:7.0f} ms  [{stage}] {leaf[:70]} <- {caller[:70]}")
print("all threads")
for stage, count in overall.most_common():
    print(f"  {stage:40} {count*interval:8.0f} ms  cpu {overall_cpu[stage]/1000:8.0f} ms")
