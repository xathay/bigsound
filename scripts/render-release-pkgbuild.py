#!/usr/bin/env python3
"""Render a portable Arch recipe from the local-build recipe with a verified source.

Usage: render-release-pkgbuild.py SOURCE_URL SHA256 OUTPUT_PATH
Use a local archive filename for an offline source bundle, or a published URL.
"""
from pathlib import Path
import re
import shlex
import shutil
import sys


def render(source, checksum, destination):
    if not re.fullmatch(r'[a-f0-9]{64}', checksum):
        raise ValueError('a verified lowercase SHA-256 digest is required')
    if any(c in source for c in '\n\r\0'):
        raise ValueError('invalid source')
    root = Path(__file__).resolve().parents[1]
    recipe = (root / 'packaging/PKGBUILD').read_text()
    start = recipe.index('# Local-build PKGBUILD:')
    end = recipe.index('# Honour CARGO_TARGET_DIR', start)
    recipe = recipe[:start] + 'source=(' + shlex.quote(source) + ')\nsha256sums=(' + shlex.quote(checksum) + ')\n\n' + recipe[end:]
    # makepkg defines srcdir only after sourcing the recipe; resolve paths
    # inside each function, including package() which may run separately.
    recipe = recipe.replace('_target="${CARGO_TARGET_DIR:-$_root/target}"\n', '')
    recipe = recipe.replace('    cd "$_root"',
                            '    local _root="$srcdir/bigsound-$pkgver"\n'
                            '    local _target="${CARGO_TARGET_DIR:-$_root/target}"\n'
                            '    cd "$_root"')
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(recipe)
    shutil.copy(root / 'packaging/bigsound.install', destination.parent / 'bigsound.install')


if __name__ == '__main__':
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    render(sys.argv[1], sys.argv[2], Path(sys.argv[3]))
