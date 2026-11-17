#!/usr/bin/env python3
"""Dev-only (B-08/B-10): RT's fit measure, Pillow `ImageFont.getlength`, for every golden hook,
caption and pill line, so the Rust port of `story_reel.fit_size` can be checked number for number.

    python3 tools/s4_pillow_widths.py [--lab $GMCLIP_LAB_MEDIA] [--font PATH] > out.tsv

Reads the 11 golden ASS files (read-only). For each Dialogue line, strips override blocks the
way RT's `plain()` does, splits on the literal `\\N`, and measures each line at the size RT
fitted it from (Hook: the plan's hook_size, default 88; Speech: 86; Pill: 50). Also measures a
fixed probe set (every printable ASCII char, kerning pairs) at 50/86/88. Output is TSV with a
`#` header naming the Pillow/FreeType/raqm versions; widths are `repr(float)` so the Rust test
can compare bit for bit.
"""
import argparse
import os, json, re, sys
from pathlib import Path
from PIL import ImageFont, features, __version__ as pil_version

GOLDEN = ['reels/%s/captions-v5.ass' % h for h in (
    'loc_086da141_h03', 'loc_086da141_h25', 'loc_086da141_h47', 'loc_4fadb756_h34', 'loc_4fadb756_h37',
    'loc_4fadb756_h48', 'loc_bd89f38b_h41', 'loc_bd89f38b_h43', 'loc_bd89f38b_h53', 'loc_bd89f38b_h88')] + [
    'h79/captions-v4.ass']
plain = lambda s: re.sub(r'{[^}]*}', '', s)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--lab', default=os.environ.get('GMCLIP_LAB_MEDIA') or str(Path.home() / 'Development/gmclip-lab-media'))
    ap.add_argument('--font', default=str(Path.home() / 'Library/Fonts/Neometric Heavy (Bold).otf'))
    a = ap.parse_args()
    root = Path(a.lab)
    fonts = {}
    font = lambda size: fonts.setdefault(size, ImageFont.truetype(a.font, size))
    print(f'# pillow {pil_version} freetype {features.version("freetype2")} raqm {features.version("raqm")} '
          f'layout {"raqm" if features.check("raqm") else "basic"}')
    print('# kind\tsource\tsize\twidth\ttext')
    rows = set()
    for rel in GOLDEN:
        p = root / rel
        hook_size = 88
        plan = p.parent / 'plan.json'
        if plan.exists():
            hook_size = json.loads(plan.read_text()).get('hook_size', 88)
        for line in p.read_text().splitlines():
            if not line.startswith('Dialogue:'):
                continue
            f = line.split(',', 9)
            style, text = f[3], f[9]
            size = {'Hook': hook_size, 'Speech': 86, 'Pill': 50}[style]
            for ln in plain(text).split('\\N'):
                rows.add(('golden', p.parent.name, size, ln))
    probes = [chr(c) for c in range(32, 127)] + ['AV', 'To', 'Ty', 'We', 'Yo', 'LT', "it's", 'five-second video',
              'BeriGame  a player is under attack', 'GENERATING  0:23', 'DONE IN  1:07']
    for size in (50, 86, 88):
        for t in probes:
            rows.add(('probe', '-', size, t))
    for kind, src, size, ln in sorted(rows):
        w = font(size).getlength(ln)
        print(f'{kind}\t{src}\t{size}\t{w!r}\t{ln}')


if __name__ == '__main__':
    main()
