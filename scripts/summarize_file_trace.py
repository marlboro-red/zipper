#!/usr/bin/env python3
"""Summarize xctrace FsSyscall XML; durations sum across threads, not wall time."""
import argparse
from collections import defaultdict
import json
from pathlib import Path
import xml.etree.ElementTree as ET

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('xml', type=Path)
parser.add_argument('--archive-name', required=True)
parser.add_argument('--file-prefix', default='record-')
args = parser.parse_args()
values = {}
summary = defaultdict(lambda: {'calls': 0, 'summed_seconds': 0.0,
                               'logical_read_bytes': 0, 'logical_write_bytes': 0})
columns = None
for event, element in ET.iterparse(args.xml, events=['end']):
    if element.tag == 'schema':
        columns = [col.findtext('mnemonic') for col in element.findall('col')]
        assert all(key in columns for key in ['duration', 'syscall', 'path', 'lreads', 'lwrites'])
    if element.tag != 'row':
        continue
    for child in element.iter():
        if 'id' in child.attrib:
            values[child.attrib['id']] = (child.text or '', child.attrib.get('fmt', ''))
    def value(key, formatted=False):
        child = element[columns.index(key)]
        raw, fmt = values.get(child.attrib.get('ref', child.attrib.get('id')), ('', ''))
        return fmt if formatted else raw
    path = value('path')
    scope = ('archive' if Path(path).name == args.archive_name else
             'output-files' if Path(path).name.startswith(args.file_prefix) else 'other')
    name = value('syscall', True)
    row = summary[(scope, name)]
    row['calls'] += 1
    row['summed_seconds'] += int(value('duration') or '0') / 1e9
    row['logical_read_bytes'] += int(value('lreads') or '0')
    row['logical_write_bytes'] += int(value('lwrites') or '0')
    element.clear()
print(json.dumps({'meaning': 'Instrumented syscall counts and summed thread durations, including cleanup; not benchmark wall time',
    'scopes': {scope: {name: row for (group, name), row in sorted(summary.items()) if group == scope}
               for scope in ['archive', 'output-files', 'other']}}, indent=2))
