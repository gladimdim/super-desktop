#!/usr/bin/env python3
"""Mirror the audited logo bundle; never download assets at runtime or build time."""
import argparse, hashlib, json, pathlib, shutil, xml.etree.ElementTree as ET
root = pathlib.Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument('--check', action='store_true')
parser.add_argument('--android', type=pathlib.Path, default=root.parent / 'OmarchyAILauncher')
args = parser.parse_args()
source = root / 'assets/logos'
target = args.android / 'android/app/src/main/assets/harness-logos'
manifest = json.loads((source / 'harness-logos.json').read_text())
files = {'harness-logos.json', 'ATTRIBUTION.md', 'LICENSES.md'}
for key, entry in manifest.items():
    for name in entry['files']:
        data = (source / name).read_bytes()
        assert hashlib.sha256(data).hexdigest() == entry['files'][name], f'Changed logo: {name}'
        if name.endswith('.png'):
            assert data.startswith(b'\x89PNG'), name
            files.add(name)
            continue
        svg = ET.fromstring(data)
        assert svg.tag.endswith('svg') and 'viewBox' in svg.attrib, name
        assert not any(e.tag.split('}')[-1] in ('script','image','text','foreignObject') for e in svg.iter()), name
        files.add(name)
if not args.check: target.mkdir(parents=True, exist_ok=True)
for stale in set(p.name for p in target.iterdir()) - files:
    if args.check: raise AssertionError(f'Unexpected Android bundle file: {stale}')
    (target / stale).unlink()
for name in sorted(files):
    if args.check:
        assert (target / name).read_bytes() == (source / name).read_bytes(), f'Android differs: {name}'
    else: shutil.copyfile(source / name, target / name)
print(f'{len(manifest)} harness logo mappings verified; {len(files)} bundled files match Android.')
