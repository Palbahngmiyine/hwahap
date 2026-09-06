#!/usr/bin/env python3
"""Set the next package version; the internal execution schema stays unchanged."""
import json
from pathlib import Path
import re
import sys

version = sys.argv[1]
if not re.fullmatch(r'\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?', version):
    raise SystemExit('usage: python3 scripts/set-version.py X.Y.Z[-prerelease]')
repo = Path(__file__).resolve().parent.parent
root = repo / 'plugins/hwahap'
old = (root / 'version.txt').read_text().strip()
(root / 'version.txt').write_text(version + '\n')
p = root / '.codex-plugin/plugin.json'
manifest = json.loads(p.read_text())
manifest['version'] = version
p.write_text(json.dumps(manifest, indent=2) + '\n')
p = root / 'runtime/Cargo.toml'
p.write_text(p.read_text().replace(f'version = "{old}"', f'version = "{version}"', 1))
p = root / 'runtime/Cargo.lock'
p.write_text(p.read_text().replace(f'name = "hwahap"\nversion = "{old}"', f'name = "hwahap"\nversion = "{version}"'))
for p in [repo / 'README.md', root / 'README.md']:
    p.write_text(p.read_text().replace(old, version))
print(f'Package version: {old} -> {version}; run python3 tests/versions.py before committing.')
