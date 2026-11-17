#!/usr/bin/env python3
"""Rewrite one track's edit list in an MP4/MOV whose `moov` follows `mdat` (dev-only, W0-05).

Used by `tools/make_corpus.sh` to build the edit-list zoo: files whose `elst` exercises
cases ffmpeg's muxer never writes (mid-GOP starts, off-frame starts, end trims, empty
edits). Because `moov` is the last top-level box, growing or shrinking it moves no
sample data and every `stco`/`co64` offset stays valid.

usage: elst_patch.py IN OUT TRACK_INDEX 'D:M[,D:M...]'
  D = segment_duration in the movie timescale, M = media_time in the media timescale
  (-1 = empty edit). Rate is always 1.0.
"""
import struct
import sys


def boxes(buf, start, end):
    off = start
    while off + 8 <= end:
        size, kind = struct.unpack(">I4s", buf[off:off + 8])
        head = 8
        if size == 1:
            size = struct.unpack(">Q", buf[off + 8:off + 16])[0]
            head = 16
        elif size == 0:
            size = end - off
        yield off, size, head, kind
        off += size


def main():
    src, dst, track, spec = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
    entries = []
    for item in spec.split(","):
        d, m = item.split(":")
        entries.append((int(d), int(m)))
    buf = bytearray(open(src, "rb").read())
    top = list(boxes(buf, 0, len(buf)))
    moov = [b for b in top if b[3] == b"moov"]
    if len(moov) != 1 or top[-1][3] != b"moov":
        sys.exit("elst_patch: moov must be the last top-level box")
    m_off, m_size, m_head, _ = moov[0]
    traks = [b for b in boxes(buf, m_off + m_head, m_off + m_size) if b[3] == b"trak"]
    t_off, t_size, t_head, _ = traks[track]
    children = list(boxes(buf, t_off + t_head, t_off + t_size))
    body = struct.pack(">BBBBI", 0, 0, 0, 0, len(entries))
    for d, m in entries:
        body += struct.pack(">IiHH", d, m, 1, 0)
    elst = struct.pack(">I4s", 8 + len(body), b"elst") + body
    edts = struct.pack(">I4s", 8 + len(elst), b"edts") + elst
    new_trak_body = bytearray()
    placed = False
    for c_off, c_size, _, kind in children:
        if kind == b"edts":
            continue
        new_trak_body += buf[c_off:c_off + c_size]
        if kind == b"tkhd" and not placed:
            new_trak_body += edts
            placed = True
    if not placed:
        sys.exit("elst_patch: track has no tkhd")
    new_trak = struct.pack(">I4s", 8 + len(new_trak_body), b"trak") + new_trak_body
    moov_body = bytearray()
    for c_off, c_size, _, kind in boxes(buf, m_off + m_head, m_off + m_size):
        if c_off == t_off:
            moov_body += new_trak
        else:
            moov_body += buf[c_off:c_off + c_size]
    new_moov = struct.pack(">I4s", 8 + len(moov_body), b"moov") + moov_body
    out = buf[:m_off] + new_moov
    open(dst, "wb").write(out)


if __name__ == "__main__":
    main()
