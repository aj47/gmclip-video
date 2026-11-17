#!/usr/bin/env python3
"""Dev-only S4 (B-10): libass references for every distinct golden text event, and SSIM of
gmclip's text raster against them.

    GMCLIP_S4_OUT=OURS cargo test --release -p gmclip-render --features lab --test s4_raster
    python3 tools/s4_compare.py OURS REFS [--ffmpeg static_ffmpeg] [--report report.tsv]

OURS holds `NNNN.tile` (premultiplied RGBA8 tiles; anchor and size in `index.tsv`) from the Rust
lab test. For each row, REFS/NNNN.npz (the reference cropped to its own coverage + 48 px, straight
RGBA8, compressed, with its canvas origin) is made (once) with ffmpeg's `ass` filter (libass) from a one-event
script: the golden file's own header and styles plus that Dialogue, shown at t = 0. vf_ass does
not write alpha, so each reference is rendered over black and over white and un-composited:
alpha = 1 − (W − B)/255, premultiplied colour = B. `static_ffmpeg` is RT's own renderer build,
so its libass is the one the goldens were burned with.

SSIM (Wang et al., 11-tap Gaussian σ 1.5, BT.709 luma) is taken over the union of both
images' coverage (alpha > 0) padded by 8 px, after compositing each over three backgrounds
(RT's #0c1118, mid grey, white); the minimum of the three is the event's score. Also reported:
the bounding-box offsets of alpha ≥ 128 (ref − ours, px) and the mean |Δalpha| in the ROI.
"""
import argparse, subprocess, sys, tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import os

import numpy as np
from scipy.ndimage import gaussian_filter

W, H = 1080, 1920
LAB = Path(os.environ.get('GMCLIP_LAB_MEDIA') or Path.home() / 'Development/gmclip-lab-media')
GOLDEN = {**{h: LAB / 'reels' / h / 'captions-v5.ass' for h in (
    'loc_086da141_h03', 'loc_086da141_h25', 'loc_086da141_h47', 'loc_4fadb756_h34', 'loc_4fadb756_h37',
    'loc_4fadb756_h48', 'loc_bd89f38b_h41', 'loc_bd89f38b_h43', 'loc_bd89f38b_h53', 'loc_bd89f38b_h88')},
          'loc_4fadb756_h79': LAB / 'h79' / 'captions-v4.ass'}
PROBE = Path('probe.ass')  # --probe FILE: rows named "probe" take their header from it
BACKGROUNDS = [(0x0c, 0x11, 0x18), (0x80, 0x80, 0x80), (0xff, 0xff, 0xff)]


def render_ref(ffmpeg, name, style, body, fontsdir=None):
    head = []
    for line in GOLDEN.get(name, PROBE).read_text().splitlines():
        head.append(line)
        if line.startswith('Format: Layer'):
            break
    script = '\n'.join(head + [f'Dialogue: 0,0:00:00.00,0:00:01.00,{style},,0,0,0,,{body}']) + '\n'
    planes = []
    with tempfile.TemporaryDirectory() as td:
        ass = Path(td) / 'one.ass'
        ass.write_text(script)
        for bg in ('black', 'white'):
            raw = subprocess.run([ffmpeg, '-v', 'error', '-f', 'lavfi', '-i', f'color=c={bg}:s={W}x{H}:r=30,format=rgb24',
                                  '-vf', f'ass={ass}' + (f':fontsdir={fontsdir}' if fontsdir else ''), '-frames:v', '1', '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-'],
                                 check=True, capture_output=True).stdout
            planes.append(np.frombuffer(raw, np.uint8).reshape(H, W, 3).astype(np.int32))
    b, w = planes
    a = np.clip(255 - (w - b).max(axis=2), 0, 255)
    rgb = np.where(a[..., None] > 0, np.clip(np.round(b * 255.0 / np.maximum(a, 1)[..., None]), 0, 255), 0)
    return np.concatenate([rgb, a[..., None]], axis=2).astype(np.uint8)


GROW = 48


def roi(x, y, w, h):
    return max(x - GROW, 0), max(y - GROW, 0), min(x + w + GROW, W), min(y + h + GROW, H)


def load_ours(path, x, y, w, h, box):
    """Our tile, un-premultiplied, placed into the ROI box (x0, y0, x1, y1) as straight RGBA."""
    x0, y0, x1, y1 = box
    t = np.fromfile(path, np.uint8).reshape(h, w, 4).astype(np.float64)
    a = t[..., 3:4]
    rgb = np.where(a > 0, t[..., :3] * 255.0 / np.maximum(a, 1), 0)
    out = np.zeros((y1 - y0, x1 - x0, 4))
    sx0, sy0 = max(x0 - x, 0), max(y0 - y, 0)
    sx1, sy1 = min(x1 - x, w), min(y1 - y, h)
    out[y - y0 + sy0:y - y0 + sy1, x - x0 + sx0:x - x0 + sx1] = np.concatenate([rgb, a], axis=2)[sy0:sy1, sx0:sx1]
    return out


def over(img, bg):
    a = img[..., 3:4] / 255.0
    return img[..., :3] * a + np.array(bg, np.float64) * (1 - a)


def luma(rgb):
    return rgb @ np.array([0.2126, 0.7152, 0.0722])


def ssim(x, y):
    c1, c2 = (0.01 * 255) ** 2, (0.03 * 255) ** 2
    f = lambda z: gaussian_filter(z, 1.5, truncate=3.5)
    mx, my = f(x), f(y)
    sxx, syy, sxy = f(x * x) - mx * mx, f(y * y) - my * my, f(x * y) - mx * my
    m = ((2 * mx * my + c1) * (2 * sxy + c2)) / ((mx * mx + my * my + c1) * (sxx + syy + c2))
    return float(m.mean())


def bbox(a):
    ys, xs = np.nonzero(a)
    return (xs.min(), ys.min(), xs.max(), ys.max()) if len(xs) else None


def compare(ours, ref):
    cov = (ours[..., 3] > 0) | (ref[..., 3] > 0)
    bb = bbox(cov)
    if bb is None:
        return 1.0, (0, 0, 0, 0), 0.0
    hh, ww = cov.shape
    x0, y0, x1, y1 = max(bb[0] - 8, 0), max(bb[1] - 8, 0), min(bb[2] + 9, ww), min(bb[3] + 9, hh)
    o, r = ours[y0:y1, x0:x1], ref[y0:y1, x0:x1]
    score = min(ssim(luma(over(o, bg)), luma(over(r, bg))) for bg in BACKGROUNDS)
    bo, br = bbox(o[..., 3] >= 128), bbox(r[..., 3] >= 128)
    off = tuple(int(b - a) for a, b in zip(bo, br)) if bo and br else (0, 0, 0, 0)
    # Alpha centroids (ours − libass, px): sub-pixel placement differences the bbox can't see.
    yy, xx = np.indices(o[..., 3].shape)
    cen = lambda a: (float((a * xx).sum() / a.sum()), float((a * yy).sum() / a.sum())) if a.sum() else (0.0, 0.0)
    (ox, oy), (rx, ry) = cen(o[..., 3]), cen(r[..., 3])
    return score, off + (round(ox - rx, 3), round(oy - ry, 3)), float(np.abs(o[..., 3] - r[..., 3]).mean())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('ours'); ap.add_argument('refs')
    ap.add_argument('--ffmpeg', default='static_ffmpeg'); ap.add_argument('--report')
    ap.add_argument('--threshold', type=float, default=0.98)
    ap.add_argument('--probe', help='ASS file the "probe" rows came from (GMCLIP_S4_ASS)')
    a = ap.parse_args()
    if a.probe:
        global PROBE
        PROBE = Path(a.probe)
    ours, refs = Path(a.ours), Path(a.refs)
    refs.mkdir(parents=True, exist_ok=True)
    rows = [l.split('\t') for l in (ours / 'index.tsv').read_text().splitlines() if l]

    def one(row):
        n, name, eid, style, x, y, w, h, body = row
        x, y, w, h = int(x), int(y), int(w), int(h)
        ref = refs / f'{n}.npz'
        if ref.exists():
            z = np.load(ref)
            r, (rx, ry) = z['rgba'], z['origin']
        else:
            full = render_ref(a.ffmpeg, name, style, body)
            bb = bbox(full[..., 3] > 0) or (0, 0, 0, 0)
            rx, ry = max(bb[0] - GROW, 0), max(bb[1] - GROW, 0)
            r = full[ry:min(bb[3] + GROW, H), rx:min(bb[2] + GROW, W)]
            np.savez_compressed(ref, rgba=r, origin=np.array([rx, ry]))
        # Compare in the union of both boxes.
        box = (min(x, rx), min(y, ry), max(x + w, rx + r.shape[1]), max(y + h, ry + r.shape[0]))
        o = load_ours(ours / f'{n}.tile', x, y, w, h, box)
        rr = np.zeros_like(o)
        rr[ry - box[1]:ry - box[1] + r.shape[0], rx - box[0]:rx - box[0] + r.shape[1]] = r
        s, off, da = compare(o, rr)
        return n, name, eid, style, s, off, da, body

    with ThreadPoolExecutor(8) as ex:
        res = list(ex.map(one, rows))
    lines = ['n\treel\tevent\tstyle\tssim\tbbox_dx0\tbbox_dy0\tbbox_dx1\tbbox_dy1\tcentroid_dx\tcentroid_dy\tmean_dalpha\ttext']
    for n, name, eid, style, s, off, da, body in res:
        lines.append(f'{n}\t{name}\t{eid}\t{style}\t{s:.5f}\t' + '\t'.join(map(str, off)) + f'\t{da:.3f}\t{body}')
    if a.report:
        Path(a.report).write_text('\n'.join(lines) + '\n')
    scores = np.array([r[4] for r in res])
    print(f'{len(res)} distinct events; SSIM min {scores.min():.4f} mean {scores.mean():.4f} median {np.median(scores):.4f}; '
          f'{(scores >= a.threshold).sum()}/{len(res)} ≥ {a.threshold}')
    for style in sorted({r[3] for r in res}):
        s = np.array([r[4] for r in res if r[3] == style])
        offs = np.array([r[5][:4] for r in res if r[3] == style])
        cens = np.array([r[5][4:] for r in res if r[3] == style])
        print(f'  {style}: n {len(s)} min {s.min():.4f} mean {s.mean():.4f}; bbox offset (ref−ours) mean '
              f'{offs.mean(axis=0).round(2).tolist()} max |·| {np.abs(offs).max(axis=0).tolist()}; '
              f'centroid (ours−libass) mean {cens.mean(axis=0).round(3).tolist()} max |·| {np.abs(cens).max(axis=0).round(3).tolist()}')
    worst = sorted(res, key=lambda r: r[4])[:8]
    for n, name, eid, style, s, off, da, body in worst:
        print(f'  worst {n} {name} {eid} {style} ssim {s:.4f} off {off} dalpha {da:.2f} {body[:60]}')



# ---------------------------------------------------------------------------------------------
# `python3 tools/s4_compare.py --metrics OURS REFS`: em_per_size and line advance, measured.

def fill_weight(img):
    """Per-pixel fill coverage: luminance × alpha (the black outline and shadow weigh 0)."""
    return luma(img[..., :3]) / 255.0 * img[..., 3] / 255.0


def line_centroids(img):
    prof = fill_weight(img).sum(axis=1)
    on = prof > 0.5
    segs, start = [], None
    for i, v in enumerate(list(on) + [False]):
        if v and start is None:
            start = i
        elif not v and start is not None:
            segs.append((start, i))
            start = None
    return [float((prof[a:b] * np.arange(a, b)).sum() / prof[a:b].sum()) for a, b in segs]


def metrics(ours, refs, ffmpeg, font, family, fontsdir=None):
    from fontTools.ttLib import TTFont
    f = TTFont(font)
    os2, upem = f['OS/2'], f['head'].unitsPerEm
    gs = f.getGlyphSet()
    from fontTools.pens.boundsPen import BoundsPen
    bp = BoundsPen(gs)
    gs[f.getBestCmap()[ord('H')]].draw(bp)
    cap = bp.bounds[3] - bp.bounds[1]
    eps_font = upem / (os2.usWinAscent + os2.usWinDescent)
    # em: an un-outlined "H" at libass size 400; the stem's column coverage is the cap height.
    golden_head = GOLDEN['loc_bd89f38b_h53'].read_text().split('[Events]')[0]
    hook = 'Style: Hook,Neometric Heavy,88,&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,-1,0,0,0,100,100,0,0,1,12,7,5'
    assert hook in golden_head, 'golden Hook style changed; the probe would measure an outlined 88'
    header = golden_head.replace(
        hook, f'Style: Hook,{family},400,&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,-1,0,0,0,100,100,0,0,1,0,0,5')
    probe = Path(tempfile.mkdtemp()) / 'probe.ass'
    probe.write_text(header + '[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n')
    global PROBE
    PROBE = probe
    img = render_ref(ffmpeg, 'probe', 'Hook', '{\\pos(540,960)}H', fontsdir).astype(np.float64)
    a = img[..., 3] / 255.0
    cols = a.sum(axis=0)
    stem = int(np.argmax(cols))
    cap_px = float(a[:, stem - 3:stem + 4].sum(axis=0).max())
    eps_libass = cap_px * upem / (cap * 400)
    print(f'em_per_size: font rule {eps_font:.5f}; libass measured {eps_libass:.5f} (cap {cap} units → {cap_px:.2f} px at size 400)')
    two = render_ref(ffmpeg, 'probe', 'Hook', '{\\pos(540,960)}H\\NH', fontsdir).astype(np.float64)
    c = line_centroids(two)
    print(f'line advance at size 400: libass {c[1] - c[0]:.3f} px (rule: = size = 400)')
    if ours is None:
        return
    # Golden two-line hooks: ours vs libass.
    for row in (ours / 'index.tsv').read_text().splitlines():
        n, name, eid, style, x, y, w, h, body = row.split('\t')
        if style != 'Hook' or '\\N' not in body:
            continue
        x, y, w, h = int(x), int(y), int(w), int(h)
        z = np.load(refs / f'{n}.npz')
        r, (rx, ry) = z['rgba'].astype(np.float64), z['origin']
        box = (min(x, rx), min(y, ry), max(x + w, rx + r.shape[1]), max(y + h, ry + r.shape[0]))
        o = load_ours(ours / f'{n}.tile', x, y, w, h, box)
        rr = np.zeros_like(o)
        rr[ry - box[1]:ry - box[1] + r.shape[0], rx - box[0]:rx - box[0] + r.shape[1]] = r
        co, cr = line_centroids(o), line_centroids(rr)
        if len(co) == 2 and len(cr) == 2:
            print(f'  {name} hook: line 1→2 ours {co[1] - co[0]:.3f} px, libass {cr[1] - cr[0]:.3f} px, '
                  f'Δ {abs((co[1] - co[0]) - (cr[1] - cr[0])):.3f} px; line 1 Δy {co[0] - cr[0]:+.3f}')
        else:
            print(f'  {name} hook: could not split lines ({len(co)}, {len(cr)})')


if __name__ == '__main__' and '--metrics' in sys.argv:
    # --metrics OURS REFS [--ffmpeg BIN]: Neometric em, line advance and the golden hooks.
    # --metrics --font FILE --family NAME [--fontsdir DIR]: em and line advance of another face.
    mp = argparse.ArgumentParser()
    mp.add_argument('--metrics', action='store_true'); mp.add_argument('dirs', nargs='*')
    mp.add_argument('--ffmpeg', default='static_ffmpeg')
    mp.add_argument('--font', default=str(Path.home() / 'Library/Fonts/Neometric Heavy (Bold).otf'))
    mp.add_argument('--family', default='Neometric Heavy'); mp.add_argument('--fontsdir')
    m = mp.parse_args()
    ours, refs = (Path(m.dirs[0]), Path(m.dirs[1])) if len(m.dirs) == 2 else (None, None)
    metrics(ours, refs, m.ffmpeg, m.font, m.family, m.fontsdir)
    sys.exit(0)


if __name__ == '__main__':
    main()
