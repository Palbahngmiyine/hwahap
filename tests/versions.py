#!/usr/bin/env python3
"""Prevent a release from mixing plugin, launcher, and Rust versions."""
import json
from pathlib import Path
import re
import tomllib

root = Path(__file__).resolve().parent.parent / 'plugins/hwahap'
version = (root / 'version.txt').read_text().strip()
assert re.fullmatch(r'\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?', version)
manifest = json.loads((root / '.codex-plugin/plugin.json').read_text())
crate = tomllib.loads((root / 'runtime/Cargo.toml').read_text())
lock = tomllib.loads((root / 'runtime/Cargo.lock').read_text())
assert version == manifest['version'] == crate['package']['version']
assert [p['version'] for p in lock['package'] if p['name'] == 'hwahap'] == [version]
assert (root / 'skills/hwahap/SKILL.md').is_file()
print(f'Plugin, launcher, Cargo manifest and lockfile agree: {version}')
