#!/usr/bin/env python3
"""Dev-only (tools/, never shipped): real coded media for the muxer lab tests (B-04, B-05).

  mux_fixtures.py video OUT.mp4 [SECONDS]
      x264 High@4.0 1080x1920@30, B-frames 2 with b-pyramid (reorder depth 2, as VT's
      B=2), closed GOP 30, BT.709 tags. Our muxer re-wraps its samples.
  mux_fixtures.py aac FEED.wav OUT_PREFIX
      AAC-LC 192k from FEED.wav twice: libavcodec (priming D = 1024) to
      OUT_PREFIX-lavc.m4a and AudioToolbox via afconvert (D = 2112) to OUT_PREFIX-at.m4a.

Uses /opt/homebrew/bin/ffmpeg (GPL build: fine here, it never ships) and /usr/bin/afconvert.
"""
import os
import subprocess
import sys

FFMPEG = os.environ.get("FFMPEG", "/opt/homebrew/bin/ffmpeg")


def run(cmd):
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def video(out, seconds="2"):
    run([FFMPEG, "-v", "error", "-y", "-f", "lavfi", "-i", f"testsrc2=size=1080x1920:rate=30:duration={seconds}",
         "-c:v", "libx264", "-profile:v", "high", "-level", "4.0", "-pix_fmt", "yuv420p",
         "-x264-params", "bframes=2:b-pyramid=normal:keyint=30:min-keyint=30:scenecut=0:open-gop=0",
         "-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv",
         "-an", out])


def aac(feed, prefix):
    run([FFMPEG, "-v", "error", "-y", "-i", feed, "-c:a", "aac", "-b:a", "192k", "-ar", "48000",
         f"{prefix}-lavc.m4a"])
    run(["/usr/bin/afconvert", "-f", "m4af", "-d", "aac@48000", "-b", "192000", "-s", "0",
         feed, f"{prefix}-at.m4a"])


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "video":
        video(sys.argv[2], *(sys.argv[3:4]))
    elif len(sys.argv) == 4 and sys.argv[1] == "aac":
        aac(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)
