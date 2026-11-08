"""Builds the NTFS corpus with mkntfs and ntfs_build, which links libntfs-3g,
then compresses each volume with xz -9.

    python3 make_corpus.py OUT_FOLDER PATH_TO_MKNTFS PATH_TO_NTFS_BUILD
"""
import os, subprocess, sys

OUT, MKNTFS, BUILD = sys.argv[1:4]

def rel(target):
    import struct
    subst = target.encode('utf-16-le')
    body = struct.pack('<HHHHI', 0, len(subst), len(subst), len(subst), 1) + subst + subst
    return (struct.pack('<IHH', 0xA000000C, len(body), 0) + body).hex()

def absolute(target):
    import struct
    subst = target.encode('utf-16-le')
    body = struct.pack('<HHHHI', 0, len(subst), len(subst), len(subst), 0) + subst + subst
    return (struct.pack('<IHH', 0xA000000C, len(body), 0) + body).hex()

def junction(target):
    import struct
    subst = target.encode('utf-16-le')
    body = struct.pack('<HHHH', 0, len(subst), len(subst) + 2, len(subst)) + subst + b'\0\0' + subst + b'\0\0'
    return (struct.pack('<IHH', 0xA0000003, len(body), 0) + body).hex()

def basic():
    lines = ['dir /folder', 'dir /folder/sub', 'file /hello.txt 13 1 t', 'file /empty 0 1 t',
             'file /folder/resident.txt 200 2 t', 'file /folder/data.bin 9000 3 r',
             'file /folder/sub/text.txt 30000 4 t', 'link /hello.txt /folder/hard.txt',
             'link /hello.txt /folder/sub/second.txt',
             'file /A-Long-File-Name.text 40 5 t', 'dos /A-Long-File-Name.text ALONGF~1.TEX',
             'dir /café-ünïcödé', 'file /café-ünïcödé/日本語.txt 20 6 t',
             'file /readonly.txt 7 7 t', 'attrib /readonly.txt 1',
             'time /hello.txt 1600000000', 'time /folder 1500000000', 'time /folder/sub/text.txt 1234567890',
             'file /gone.txt 10 8 t', 'delete /gone.txt', 'dir /wide']
    for i in range(400):
        lines.append(f'file /wide/entry{i:04d}.txt {i % 7} {i} t')
    path = ''
    for depth in range(24):
        path += f'/d{depth}'
        lines.append(f'dir {path}')
    lines.append(f'file {path}/bottom.txt 6 9 t')
    return lines

def compressed():
    return ['cdir /packed', 'file /packed/text.txt 150000 1 t', 'file /packed/random.bin 70000 2 r',
            'file /packed/small.txt 300 3 t', 'file /packed/unit.txt 65536 4 t',
            'file /packed/one-chunk.txt 4096 5 t', 'file /plain.txt 5000 6 t',
            'cdir /packed/inner', 'file /packed/inner/deep.txt 20000 7 t']

def fragmented():
    return ['frag /one.bin /two.bin 700000 1024 1 t', 'frag /three.bin /four.bin 40000 2048 2 t',
            'dir /folder', 'file /folder/after.txt 3000 3 t']

def links():
    return ['dir /folder', 'file /folder/data.bin 100 1 r', 'file /hello.txt 13 2 t',
            'file /relative.txt 0 1 t', 'reparse /relative.txt ' + rel('hello.txt'),
            'file /folder/up.txt 0 1 t', 'reparse /folder/up.txt ' + rel('..\\hello.txt'),
            'file /absolute.txt 0 1 t', 'reparse /absolute.txt ' + absolute('\\??\\C:\\folder\\data.bin'),
            'dir /junction', 'reparse /junction ' + junction('\\??\\C:\\folder')]

def sparse():
    return ['sparse /holes.bin 300000 0:5000,200000:9000', 'sparse /tail.bin 100000 90000:10000',
            'sparse /empty-holes.bin 80000 ', 'file /plain.txt 100 1 t']

def streams():
    return ['file /hello.txt 13 1 t', 'ads /hello.txt Zone.Identifier 30 2']

def small_clusters():
    return ['dir /folder', 'file /folder/data.bin 3000 1 r', 'file /text.txt 9000 2 t',
            'cdir /packed', 'file /packed/text.txt 40000 3 t', 'link /text.txt /folder/link.txt']

IMAGES = [
    ('ntfs3g-basic', 4, [], basic),
    ('ntfs3g-compressed', 4, [], compressed),
    ('ntfs3g-fragmented', 4, ['-c', '1024'], fragmented),
    ('ntfs3g-links', 2, [], links),
    ('ntfs3g-sparse', 2, [], sparse),
    ('ntfs3g-streams', 2, [], streams),
    ('ntfs3g-512-clusters', 2, ['-c', '512'], small_clusters),
    ('ntfs3g-4096-sectors', 4, ['-s', '4096', '-c', '4096'], small_clusters),
]

os.makedirs(OUT, exist_ok=True)
for name, megabytes, options, script in IMAGES:
    image = os.path.join(OUT, name + '.ntfs')
    if os.path.exists(image):
        os.remove(image)
    subprocess.run(['mkfile', '-n', f'{megabytes}m', image], check=True)
    subprocess.run([MKNTFS, '-F', '-f', '-q', '-L', name[:16], *options, image], check=True,
                   stderr=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
    text = '\n'.join(script()) + '\n'
    if name == 'ntfs3g-sparse':
        text = text.replace('sparse /empty-holes.bin 80000 \n', '')
    subprocess.run([BUILD, image], input=text.encode(), check=True)
    subprocess.run(['xz', '-9', '-k', '-f', image], check=True)
    print('  ', os.path.getsize(image + '.xz'))
