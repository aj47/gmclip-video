#!/usr/bin/env python3
"""Dev-only (B-10 / S4): rank OFL heavy faces by metric match to Neometric Heavy under the
per-font em rule (SPEC Conventions), to pick the bundled CI face.

    python3 tools/s4_font_match.py WIDTHS_TSV REF.otf CAND.ttf[@wght] ...

For every golden line in WIDTHS_TSV (tools/s4_pillow_widths.py output) at its libass size S,
each face is measured at em = S * unitsPerEm / (usWinAscent + usWinDescent) with Pillow/raqm
(HarfBuzz) and compared with the reference face. Variable faces are instanced at `wght`
(default: the axis maximum) with fontTools. Prints one row per face, best first:
  width ratio (geometric mean of candidate/reference line widths, `kern` off as libass), its spread (std of the log
  ratio), cap-height and x-height ratios (in libass px per size unit), and a score
  = |log w| + std + |log cap| + |log x|.
"""
import math, sys, tempfile
from pathlib import Path
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer
from PIL import ImageFont

SCALE = 8  # measure at 8x the em to keep 26.6 rounding out of the comparison


def load(spec):
    path, _, w = spec.partition('@')
    f = TTFont(path)
    if 'fvar' in f:
        axis = next(a for a in f['fvar'].axes if a.axisTag == 'wght')
        wght = float(w) if w else axis.maxValue
        f = instancer.instantiateVariableFont(f, {'wght': wght, **{a.axisTag: a.defaultValue for a in f['fvar'].axes if a.axisTag != 'wght'}})
        tmp = tempfile.NamedTemporaryFile(suffix='.ttf', delete=False)
        f.save(tmp.name)
        path, label = tmp.name, f'{Path(spec.split("@")[0]).name}@{wght:g}'
        f = TTFont(path)
    else:
        label = Path(path).name
    os2, upem = f['OS/2'], f['head'].unitsPerEm
    span = os2.usWinAscent + os2.usWinDescent
    return {'label': label, 'path': path, 'eps': upem / span,
            'cap': getattr(os2, 'sCapHeight', 0) / span, 'x': getattr(os2, 'sxHeight', 0) / span,
            'weight': os2.usWeightClass, 'fonts': {}}


def width(face, text, size):
    em = size * face['eps'] * SCALE
    key = round(em, 6)
    fnt = face['fonts'].get(key) or face['fonts'].setdefault(key, ImageFont.truetype(face['path'], em))
    # libass shapes ASS without kerning (S4), so measure without it too.
    return fnt.getlength(text, features=['-kern']) / SCALE


def main():
    tsv, ref, cands = sys.argv[1], sys.argv[2], sys.argv[3:]
    lines = []
    for row in Path(tsv).read_text().splitlines():
        if row.startswith('#'):
            continue
        kind, _, size, _, text = row.split('\t', 4)
        if kind == 'golden' and text.strip():
            lines.append((int(size), text))
    r = load(ref)
    rw = [width(r, t, s) for s, t in lines]
    print(f'reference {r["label"]}: em_per_size {r["eps"]:.4f} cap {r["cap"]:.4f} x {r["x"]:.4f} weight {r["weight"]}; {len(lines)} lines')
    rows = []
    for c in cands:
        f = load(c)
        logs = [math.log(width(f, t, s) / w) for (s, t), w in zip(lines, rw) if w > 0]
        mu = sum(logs) / len(logs)
        sd = math.sqrt(sum((x - mu) ** 2 for x in logs) / len(logs))
        lc, lx = math.log(f['cap'] / r['cap']), math.log(f['x'] / r['x'])
        rows.append((abs(mu) + sd + abs(lc) + abs(lx), f, math.exp(mu), sd, math.exp(lc), math.exp(lx)))
    print('score\tface\tem_per_size\tweight\twidth\tspread\tcap\tx-height')
    for score, f, w, sd, cap, x in sorted(rows, key=lambda r: r[0]):
        print(f'{score:.4f}\t{f["label"]}\t{f["eps"]:.4f}\t{f["weight"]}\t{w:.4f}\t{sd:.4f}\t{cap:.4f}\t{x:.4f}')


if __name__ == '__main__':
    main()
