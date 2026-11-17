# RenderPlan v0 (schema `gmclip.renderplan/0`)

Origin: `critique/round-3/m0-backlog.md` §2, adopted by SPEC v4 §6.5; amended for SPEC v5 by `critique/round-4/companion-coherence.md` (COMP) and `critique/round-4/response.md`. Normative for M0; frozen before any converter/renderer ticket starts, on AJ's go. §2.3–§2.4 amended by `decisions/0003-rp-v0-corrections.md` (ASS frame rule, RT's centiseconds, stills). "M0-nn" ids refer to `critique/round-3/m0-backlog.md` §1 (informative). Validation results are in §2.7.

## 2. RenderPlan v0

### 2.1 Normative rules
1. **Integers only.** All times are integers: output frames (`f`), source presentation-frame indices, and audio samples at the source's rate. There are no seconds in a RenderPlan, except inside `provenance`. Every `range` is `[start, end)` with `start < end` (validator). Whoever writes a plan has already quantised: the M1 planner by SPEC §4.1.1's one tie rule (nearest frame, ties to the earlier), the RT converter by carrying RT's own integer frames (§2.4, "Rounding").
2. **Tiling.** `parts[].out` ranges tile `[0, output.frames)` in array order, with no gaps or overlaps. Every `text_events[].out` lies inside `[0, output.frames)`.
3. **Picture.** Output frame `f` of part `p` shows presentation index `p.picture.first_frame + (f − p.out[0])` of `p.picture.source`. A presentation index is the 0-based position of a frame in PTS order after edit lists (never the MP4 decode-order sample number). v0 requires a CFR source whose nominal rate equals `output.fps`; otherwise the result is `Unsupported`. `expect_pts`, when present, must equal that frame's PTS (`Invalid` otherwise). This catches index drift.
4. **Frame rate and audio.** `output.fps` is an integer rate at v0: `den` MUST be 1 and `num` ∈ {24, 25, 30, 48, 50, 60}. NTSC rates (`30000/1001` etc.) are `Unsupported` until audio-sample rounding for them is specified in `/1`. Audio is 48 kHz (`rate = 48000`). Sample counts telescope on the output clock: with `S(f) = f · 48000 / fps.num` (an integer for every allowed rate), part `p` contributes samples `[first_sample, first_sample + S(out[1]) − S(out[0]))` of `p.audio.source`, counted after edit-list and priming removal. At 30 fps that is `n × 1600`. A source's `audio_track` (optional, added within `/0` by rule 12, decision 0009) names the container track (MP4 `track_ID`, Matroska `TrackNumber`) every part's audio from that source is read from; absent, it is the source's first audio track. A plan naming a track the file lacks is `Invalid{/sources/i/audio_track}` at open.
5. **Layers.**
   - `crop` is in source display pixels (float allowed) and `place` is in canvas pixels.
   - Each layer draws `crop` resampled (Lanczos-3) into `place`. `|place.h/place.w − crop.h/crop.w| / (crop.h/crop.w) ≤ 0.005` (`Invalid` otherwise).
   - Draw order is ascending `z`, then array order.
   - `time_map.hold.frame` and `freeze_first` select a frame of the layer's `picture_source` (default: the part's). `hold.frame` is an absolute presentation index and may lie outside the part.
   - `matte {kind: roto, model, soften?, strokes?}` (decision 0013) multiplies the layer's alpha, in source pixels before the resample, by a person mask: the mask track of the crop's integer bounds over the part's frames (one frame for `hold`/`freeze_first`), softened by `soften` source pixels (two box passes), then each stroke in order (`add` raises the mask to the brush, `subtract` lowers it; `radius` and `points` in source pixels, `feather` a fraction of the radius). Which mask a renderer used — an accurate track, or a draft — is recorded on the export (`draft_mattes`); preview and export of the same track are the same pixels.
   - `role` is a string: a SPEC `RegionRole` (`cam`, `screen`, `chat`, `alerts`, `banner`, `private`, `captions_burned`), `detail` (RT detail layers: a second crop of the screen region), or `other`. The renderer ignores it; lints and the M1 planner read it.
6. **Text events.**
   - An event is visible on output frames `[out[0], out[1])` and is drawn above all layers, ordered by `layer`, then array order. v0 cannot draw text below a layer (§2.6).
   - `pos` is the anchor point for `align` (numpad convention: 5 = centre of the text block, 7 = top-left).
   - **`size` is final.** Every event carries its own `size` (required). Renderers never fit, wrap or shrink text. Fit-shrink is resolved before the plan is written: the RT converter uses RT's own measure (§2.4, "Fit"); gmclip's planner (M1) uses the per-font em rule of SPEC Conventions. `text_styles.<name>.size` is the style's nominal size and is not used when drawing an event.
   - Font `size` is in ASS units at PlayResY = `output.canvas.h`. The em in pixels is `size × fonts[].em_per_size`.
   - `outline.width`, `shadow.dx/dy/blur`, `box.pad_x/pad_y/radius`, `letter_spacing` and `pos` are canvas pixels (ASS script pixels at PlayResX/Y = canvas, `ScaledBorderAndShadow yes`). They are never em-scaled.
   - Line advance = (usWinAscent + usWinDescent) / unitsPerEm × em, which is exactly `size` (the libass rule); hhea `lineGap` is ignored. S4 verifies this against libass on the two-line golden hooks.
   - Runs carry RGBA fills (default: the style's `fill`); `\n` breaks lines; there is no automatic wrapping.
7. **Audio post-processing.**
   - Order: part gain → part fades → sum → envelopes → loudness.
   - **De-click only at discontinuous joins.** A join between parts `p` and `p+1` is *continuous* when both use the same audio source and `p+1.audio.first_sample = p.audio.first_sample + (S(p.out[1]) − S(p.out[0]))`. The validator returns `Invalid{path}` for a non-null `fade_out` on `p` or `fade_in` on `p+1` at a continuous join. Fades are allowed at discontinuous joins and at the cut's first and last sample.
   - **Crossfades are not expressible in v0.** A part contributes exactly its own samples (rule 4); there is no overlap field (§2.6).
   - Envelopes: `curve: linear_db` ramps in dB, linearly over `ramp_in_samples` and `ramp_out_samples`, inside `[s0, s1)` of the output sample clock. Envelopes may not overlap.
8. **Loudness.** `static` measures integrated loudness (BS.1770-4), applies one gain, runs a 4× oversampled lookahead true-peak limiter at `true_peak_dbtp`, then re-measures. `as_recorded` applies none of this.
9. **`render_sig`.** BLAKE3 over the RFC 8785 canonical JSON of the plan with what changes no pixel or sample removed (decision 0004): the top-level `meta`, every `provenance` object, every string `path` member and `sources[].fingerprint.mtime_ns`; every other non-integer number is quantised to 1/1024 (px, dB or ratio; round half to even). The canonical bytes are followed by the raw 32-byte SHA-256 digest of each source (`fingerprint.sha256`) and then each font (`sha256`), in array order; the result is written as lowercase hex. A source without `fingerprint.sha256` makes `render_sig` absent, and the export record says why. The same cut therefore signs the same on every machine and whichever writer (RT converter or M1 planner) produced it. The canonical form stays RFC 8785 JSON of the `/0` schema at every gate; the model's CBOR is its storage form and is never used for `render_sig`. (SPEC §4.4 defines `render_sig` the same way.) Reference vector: `schemas/examples/h79-parts-1-3.renderplan.json` → `595ef8b322511093d5f609b3ae7430ea4ef57a213b2af5ddd32f603c9011a5c7`.
10. **Referential integrity (validator, not schema).** Every `parts[].picture.source`, `parts[].audio.source` and `layers[].picture_source` names a `sources[].id`, and every `text_styles.<name>.font` names a `fonts[].id`; every `text_events[].style` names a key of `text_styles`; ids are unique within `sources`, `fonts`, `parts`, each part's `layers`, `text_events` and `audio.envelopes`. Violations are `Invalid{path}`. The schema itself requires `fonts` and `text_styles` whenever `text_events` is present (`dependentRequired`).
11. **Closed objects.** Every record object in the schema has `additionalProperties: false`, so a misspelt or unknown key is `Invalid{path}`, never silently ignored. The one map-shaped object, `text_styles` (keyed by style name), constrains its keys with `propertyNames` and its values with the closed `text_style` schema.
12. **Evolution.** Optional fields inside existing objects may be added within `/0` by updating `schemas/renderplan@0.json`; a renderer that predates the field rejects plans using it (`Invalid{path}`), which is the intended failure. New top-level keys, new values in closed enums, or changed semantics need `gmclip.renderplan/N+1`, and the renderer accepts N and N+1 for two minor releases (as R-CMD-5).

### 2.2 JSON Schema (draft 2020-12)

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "urn:gmclip:renderplan:0",
  "title": "gmclip.renderplan/0",
  "type": "object",
  "required": ["schema", "output", "background", "sources", "parts", "audio"],
  "additionalProperties": false,
  "dependentRequired": { "text_events": ["fonts", "text_styles"] },
  "properties": {
    "schema": { "const": "gmclip.renderplan/0" },
    "meta": {
      "type": "object", "additionalProperties": false,
      "properties": {
        "title": { "type": "string" }, "hook_id": { "type": "string" }, "content_id": { "type": "string" },
        "covers": { "type": "array", "items": { "type": "string" }, "description": "the content ids of the cuts merged into this one" },
        "generator": { "type": "object", "additionalProperties": false, "required": ["name", "version"],
                       "properties": { "name": { "type": "string" }, "version": { "type": "string" } } },
        "inputs": { "type": "array", "items": { "type": "object", "additionalProperties": false, "required": ["role", "path"],
          "properties": { "role": { "enum": ["reference_mp4", "model"] },
                          "path": { "type": "string" }, "sha256": { "$ref": "#/$defs/sha256" } } } },
        "import_report": { "type": "array", "items": { "type": "string" } }
      }
    },
    "output": {
      "type": "object", "additionalProperties": false,
      "required": ["canvas", "fps", "frames", "video", "audio"],
      "properties": {
        "canvas": { "type": "object", "additionalProperties": false, "required": ["w", "h"],
                    "properties": { "w": { "$ref": "#/$defs/even" }, "h": { "$ref": "#/$defs/even" } } },
        "fps": { "type": "object", "additionalProperties": false, "required": ["num", "den"],
                 "properties": { "num": { "enum": [24, 25, 30, 48, 50, 60] }, "den": { "const": 1 } } },
        "frames": { "type": "integer", "minimum": 1 },
        "color": { "type": "object", "additionalProperties": false,
                   "properties": { "matrix": { "const": "bt709" }, "transfer": { "const": "bt709" }, "primaries": { "const": "bt709" }, "range": { "enum": ["limited", "full"] } } },
        "video": { "type": "object", "additionalProperties": false, "required": ["codec"], "properties": {
          "codec": { "const": "h264" }, "profile": { "enum": ["high", "main", "constrained_baseline"] }, "level": { "type": "string" },
          "bframes": { "type": "integer", "minimum": 0, "maximum": 2 }, "gop_frames": { "type": "integer", "minimum": 1 }, "closed_gop": { "type": "boolean" },
          "rate": { "type": "object", "additionalProperties": false, "required": ["mode"],
                    "properties": { "mode": { "const": "vbr" }, "target_bps": { "type": "integer", "minimum": 1 }, "max_bps": { "type": "integer", "minimum": 1, "maximum": 25000000 } } },
          "timescale": { "type": "integer", "minimum": 1 } } },
        "audio": { "type": "object", "additionalProperties": false, "required": ["codec", "rate", "channels"], "properties": {
          "codec": { "const": "aac_lc" }, "rate": { "const": 48000 }, "channels": { "const": 2 }, "bitrate": { "type": "integer", "minimum": 1 } } },
        "container": { "type": "object", "additionalProperties": false,
                       "properties": { "moov_first": { "type": "boolean" }, "edit_lists": { "const": false } } }
      }
    },
    "background": { "oneOf": [
      { "type": "object", "additionalProperties": false, "required": ["kind", "rgb"],
        "properties": { "kind": { "const": "colour", "description": "SPEC 'solid colour'" }, "rgb": { "$ref": "#/$defs/rgb" } } },
      { "type": "object", "additionalProperties": false, "required": ["kind", "stops"],
        "properties": { "kind": { "const": "plate" }, "stops": { "type": "array", "minItems": 2, "items": { "$ref": "#/$defs/rgb" } } } } ] },
    "sources": { "type": "array", "minItems": 1, "items": {
      "type": "object", "additionalProperties": false, "required": ["id", "path"],
      "properties": {
        "id": { "$ref": "#/$defs/id" }, "path": { "type": "string" },
        "audio_track": { "type": "integer", "minimum": 1, "description": "the container track id parts' audio is read from (decision 0009); absent = the first audio track" },
        "fingerprint": { "type": "object", "additionalProperties": false,
                         "properties": { "size": { "type": "integer", "minimum": 0 }, "mtime_ns": { "type": "integer" }, "sha256": { "$ref": "#/$defs/sha256" } } },
        "expect": { "type": "object", "additionalProperties": false, "properties": {
          "video": { "type": "object", "additionalProperties": false,
                     "properties": { "codec": { "enum": ["h264", "hevc", "vp9", "av1"] }, "w": { "type": "integer" }, "h": { "type": "integer" }, "fps": { "$ref": "#/$defs/rational" }, "timescale": { "type": "integer" } } },
          "audio": { "type": "object", "additionalProperties": false,
                     "properties": { "codec": { "type": "string" }, "rate": { "type": "integer" }, "channels": { "type": "integer" } } } } } } } },
    "fonts": { "type": "array", "items": { "type": "object", "additionalProperties": false, "required": ["id", "path", "sha256", "em_per_size"],
      "properties": { "id": { "$ref": "#/$defs/id" }, "family": { "type": "string" }, "style": { "type": "string" }, "path": { "type": "string" },
                      "sha256": { "$ref": "#/$defs/sha256" },
                      "em_per_size": { "type": "number", "exclusiveMinimum": 0, "maximum": 2, "description": "unitsPerEm/(usWinAscent+usWinDescent); hhea ascent−descent when OS/2 is absent; verified in S4" } } } },
    "text_styles": { "type": "object", "propertyNames": { "$ref": "#/$defs/id" }, "additionalProperties": { "$ref": "#/$defs/text_style" } },
    "parts": { "type": "array", "minItems": 1, "items": { "$ref": "#/$defs/part" } },
    "text_events": { "type": "array", "items": { "$ref": "#/$defs/text_event" } },
    "audio": {
      "type": "object", "additionalProperties": false, "required": ["loudness"],
      "properties": {
        "envelopes": { "type": "array", "items": { "type": "object", "additionalProperties": false,
          "required": ["id", "out_samples", "gain_db", "ramp_in_samples", "ramp_out_samples", "curve"],
          "properties": { "id": { "$ref": "#/$defs/id" }, "out_samples": { "$ref": "#/$defs/range" }, "gain_db": { "type": "number", "minimum": -30, "maximum": 24 },
                          "ramp_in_samples": { "type": "integer", "minimum": 1 }, "ramp_out_samples": { "type": "integer", "minimum": 1 }, "curve": { "const": "linear_db" },
                          "provenance": { "$ref": "#/$defs/provenance" } } } },
        "loudness": { "oneOf": [
          { "type": "object", "additionalProperties": false, "required": ["mode"], "properties": { "mode": { "const": "as_recorded" } } },
          { "type": "object", "additionalProperties": false, "required": ["mode", "integrated_lufs", "true_peak_dbtp"],
            "properties": { "mode": { "const": "static" }, "integrated_lufs": { "type": "number" }, "true_peak_dbtp": { "type": "number", "maximum": 0 } } } ] }
      }
    },
    "checks": { "type": "object", "additionalProperties": false, "properties": {
      "reference": { "type": "object", "additionalProperties": false,
                     "properties": { "path": { "type": "string" }, "sha256": { "$ref": "#/$defs/sha256" }, "frames": { "type": "integer", "minimum": 1 } } },
      "safe_zones": { "enum": ["instagram"], "description": "TikTok/Shorts/YouTube zones join in /1 (S3-mac measures them)" } } }
  },
  "$defs": {
    "id": { "type": "string", "pattern": "^[A-Za-z0-9_.:-]{1,96}$" },
    "sha256": { "type": "string", "pattern": "^[0-9a-f]{64}$" },
    "even": { "type": "integer", "minimum": 2, "multipleOf": 2 },
    "rational": { "type": "object", "additionalProperties": false, "required": ["num", "den"],
                  "properties": { "num": { "type": "integer", "minimum": 1 }, "den": { "type": "integer", "minimum": 1 } } },
    "range": { "type": "array", "prefixItems": [{ "type": "integer", "minimum": 0 }, { "type": "integer", "minimum": 1 }], "minItems": 2, "maxItems": 2,
               "description": "[start, end) integers; start < end is a validator rule" },
    "rgb": { "type": "string", "pattern": "^#[0-9a-fA-F]{6}$" },
    "rgba": { "type": "string", "pattern": "^#[0-9a-fA-F]{8}$" },
    "point": { "type": "object", "additionalProperties": false, "required": ["x", "y"], "properties": { "x": { "type": "number" }, "y": { "type": "number" } } },
    "rect": { "type": "object", "additionalProperties": false, "required": ["x", "y", "w", "h"],
              "properties": { "x": { "type": "number" }, "y": { "type": "number" }, "w": { "type": "number", "exclusiveMinimum": 0 }, "h": { "type": "number", "exclusiveMinimum": 0 } } },
    "provenance": { "type": "object", "additionalProperties": false, "required": ["kind"], "properties": {
      "kind": { "enum": ["import", "measurement", "transcript", "ocr", "human", "agent"], "description": "agent = an agent-typed value with no other provenance" },
      "detail": { "type": "object", "additionalProperties": false, "properties": {
        "file": { "type": "string" }, "key": { "type": "string" }, "measurement": { "type": "string" },
        "words": { "type": "array", "items": { "type": "string" } }, "note": { "type": "string" } } } } },
    "fade": { "oneOf": [{ "type": "null" },
      { "type": "object", "additionalProperties": false, "required": ["samples", "shape"],
        "properties": { "samples": { "type": "integer", "minimum": 1 }, "shape": { "enum": ["linear", "raised_cosine"] } } }] },
    "time_map": { "oneOf": [
      { "type": "object", "additionalProperties": false, "required": ["mode"], "properties": { "mode": { "const": "live" } } },
      { "type": "object", "additionalProperties": false, "required": ["mode"], "properties": { "mode": { "const": "freeze_first" } } },
      { "type": "object", "additionalProperties": false, "required": ["mode", "frame"],
        "properties": { "mode": { "const": "hold" }, "frame": { "type": "integer", "minimum": 0 }, "provenance": { "$ref": "#/$defs/provenance" } } } ] },
    "layer": {
      "type": "object", "additionalProperties": false, "required": ["id", "role", "crop", "place", "z"],
      "properties": {
        "id": { "$ref": "#/$defs/id" },
        "role": { "type": "string", "pattern": "^[a-z_]{1,32}$", "description": "SPEC RegionRole, detail, or other (rule 5)" },
        "picture_source": { "$ref": "#/$defs/id" },
        "crop": { "$ref": "#/$defs/rect" }, "place": { "$ref": "#/$defs/rect" },
        "z": { "type": "integer" }, "visible": { "type": "boolean", "default": true },
        "opacity": { "type": "number", "minimum": 0, "maximum": 1, "default": 1 },
        "resample": { "const": "lanczos3" },
        "time_map": { "$ref": "#/$defs/time_map" },
        "matte": { "$ref": "#/$defs/matte", "description": "a roto cut-out (decision 0013)" } } },
    "matte": {
      "type": "object", "additionalProperties": false, "required": ["kind", "model"],
      "properties": {
        "kind": { "const": "roto" },
        "model": { "type": "string", "description": "segmentation model family, e.g. apple_vision" },
        "soften": { "type": "number", "minimum": 0, "maximum": 32, "default": 0, "description": "edge softening radius, source pixels" },
        "strokes": { "type": "array", "maxItems": 256, "items": { "$ref": "#/$defs/matte_stroke" } } } },
    "matte_stroke": {
      "type": "object", "additionalProperties": false, "required": ["mode", "radius", "points"],
      "properties": {
        "mode": { "enum": ["add", "subtract"] },
        "radius": { "type": "number", "exclusiveMinimum": 0, "maximum": 4096, "description": "source pixels" },
        "feather": { "type": "number", "minimum": 0, "maximum": 1, "default": 0 },
        "points": { "type": "array", "minItems": 1, "maxItems": 4096, "items": { "$ref": "#/$defs/point" } } } },
    "part": {
      "type": "object", "additionalProperties": false, "required": ["id", "out", "picture", "audio", "layers"],
      "properties": {
        "id": { "$ref": "#/$defs/id" }, "label": { "type": "string" }, "role": { "type": "string" },
        "out": { "$ref": "#/$defs/range" },
        "picture": { "type": "object", "additionalProperties": false, "required": ["source", "first_frame"], "properties": {
          "source": { "$ref": "#/$defs/id" }, "first_frame": { "type": "integer", "minimum": 0 },
          "expect_pts": { "type": "object", "additionalProperties": false, "required": ["ticks", "timescale"],
                          "properties": { "ticks": { "type": "integer" }, "timescale": { "type": "integer", "minimum": 1 } } } } },
        "audio": { "type": "object", "additionalProperties": false, "required": ["source", "first_sample", "rate"], "properties": {
          "source": { "$ref": "#/$defs/id" }, "first_sample": { "type": "integer", "minimum": 0 }, "rate": { "const": 48000 },
          "gain_db": { "type": "number", "minimum": -30, "maximum": 12, "default": 0 },
          "fade_in": { "$ref": "#/$defs/fade" }, "fade_out": { "$ref": "#/$defs/fade" } } },
        "native": { "type": "object", "additionalProperties": false, "description": "informational: the stream-clock frame range this part came from",
                    "properties": { "frames": { "$ref": "#/$defs/range" }, "fps": { "$ref": "#/$defs/rational" } } },
        "layers": { "type": "array", "items": { "$ref": "#/$defs/layer" } } } },
    "text_style": {
      "type": "object", "additionalProperties": false, "required": ["font", "size"],
      "properties": {
        "font": { "$ref": "#/$defs/id", "description": "a fonts[].id (rule 10)" },
        "size": { "type": "number", "exclusiveMinimum": 0, "description": "nominal; ASS units at PlayResY = canvas.h" },
        "fill": { "$ref": "#/$defs/rgba" },
        "outline": { "type": "object", "additionalProperties": false,
                     "properties": { "width": { "type": "number", "minimum": 0 }, "colour": { "$ref": "#/$defs/rgba" }, "join": { "const": "round" } } },
        "shadow": { "type": "object", "additionalProperties": false,
                    "properties": { "dx": { "type": "number" }, "dy": { "type": "number" }, "blur": { "type": "number", "minimum": 0, "default": 0 },
                                    "colour": { "$ref": "#/$defs/rgba" }, "of": { "enum": ["fill_and_outline", "fill"] } } },
        "box": { "type": "object", "additionalProperties": false,
                 "properties": { "colour": { "$ref": "#/$defs/rgba" }, "pad_x": { "type": "number", "minimum": 0 }, "pad_y": { "type": "number", "minimum": 0 },
                                 "radius": { "type": "number", "minimum": 0, "default": 0 }, "geometry": { "const": "libass_bs3" } } },
        "letter_spacing": { "type": "number", "default": 0 },
        "align": { "type": "integer", "minimum": 1, "maximum": 9 } } },
    "text_event": {
      "type": "object", "additionalProperties": false, "required": ["id", "out", "style", "size", "pos", "runs", "role"],
      "properties": {
        "id": { "$ref": "#/$defs/id" }, "out": { "$ref": "#/$defs/range" }, "layer": { "type": "integer", "default": 0 },
        "role": { "enum": ["caption", "text_hook", "section_pill", "run_clock", "note"] },
        "style": { "$ref": "#/$defs/id", "description": "a text_styles key (rule 10)" },
        "pos": { "$ref": "#/$defs/point" },
        "align": { "type": "integer", "minimum": 1, "maximum": 9 },
        "size": { "type": "number", "exclusiveMinimum": 0, "description": "final size, ASS units (\\fs); renderers never refit (rule 6)" },
        "runs": { "type": "array", "minItems": 1, "items": { "type": "object", "additionalProperties": false, "required": ["text"],
                  "properties": { "text": { "type": "string" }, "fill": { "$ref": "#/$defs/rgba" } } } },
        "provenance": { "$ref": "#/$defs/provenance" } } }
  }
}
```

### 2.3 Example: h79 v4, parts 1–3 as a standalone plan

This is a complete, valid plan: the first three parts of h79 v4 (`loc_4fadb756_h79`), 397 frames, with every style and font it uses defined and every frame tiled. Its numbers are copied from `production/h79-v3-2026-10-04/render-receipt.json` (segments 0–2) and `captions-v4.ass`. The full 1807-frame plan is `schemas/examples/h79-v4.renderplan.json`, written by B-13 and one of W0-07's examples. In the full plan the pill runs to frame 774; here it is clipped to the excerpt.

```json
{ "schema": "gmclip.renderplan/0",
  "meta": { "hook_id": "loc_4fadb756_h79", "content_id": "cnt_20261003_33ecfcc4d7", "title": "My PC made this in 67 seconds (parts 1-3)" },
  "output": { "canvas": { "w": 1080, "h": 1920 }, "fps": { "num": 30, "den": 1 }, "frames": 397,
              "color": { "matrix": "bt709", "transfer": "bt709", "primaries": "bt709", "range": "limited" },
              "video": { "codec": "h264", "profile": "high", "level": "4.0", "bframes": 2, "gop_frames": 30, "closed_gop": true,
                         "rate": { "mode": "vbr", "target_bps": 16000000, "max_bps": 24000000 }, "timescale": 15360 },
              "audio": { "codec": "aac_lc", "rate": 48000, "channels": 2, "bitrate": 192000 },
              "container": { "moov_first": true, "edit_lists": false } },
  "background": { "kind": "colour", "rgb": "#0c1118" },
  "sources": [ { "id": "mkv1440", "path": "streams/jMcAmooE53I-source.mkv",
                 "fingerprint": { "size": 1132838200, "sha256": "13ed3a2671ad918f9e1f79dea18e2b44d30a4293ce106ae44e3bbbfa3f2ddb48" },
                 "expect": { "video": { "codec": "vp9", "w": 2560, "h": 1440, "fps": { "num": 30, "den": 1 } } } },
               { "id": "proxy", "path": "preview/loc_4fadb756.proxy.mp4",
                 "fingerprint": { "size": 614017711, "sha256": "ad8e69cda5ba9dc10624bbd19a86cb9544a2d7c458952b6e53bf316e78deaa2d" },
                 "expect": { "audio": { "codec": "aac", "rate": 48000, "channels": 2 } } } ],
  "fonts": [ { "id": "neometric-heavy", "family": "Neometric", "style": "Heavy", "path": "fonts/Neometric Heavy (Bold).otf",
               "sha256": "9ece81e8896ebc740c5256985dcbbeee0251011e6df73f5e637c8e599ad6a76e", "em_per_size": 0.8332 } ],
  "text_styles": {
    "Speech": { "font": "neometric-heavy", "size": 86, "fill": "#FFFFFFFF", "outline": { "width": 12, "colour": "#000000FF", "join": "round" },
                "shadow": { "dx": 7, "dy": 7, "blur": 0, "colour": "#000000FF", "of": "fill_and_outline" }, "align": 5 },
    "Hook":   { "font": "neometric-heavy", "size": 88, "fill": "#FFFFFFFF", "outline": { "width": 12, "colour": "#000000FF", "join": "round" },
                "shadow": { "dx": 7, "dy": 7, "blur": 0, "colour": "#000000FF", "of": "fill_and_outline" }, "align": 5 },
    "Pill":   { "font": "neometric-heavy", "size": 50, "fill": "#FFFFFFFF",
                "box": { "colour": "#101010CF", "pad_x": 16, "pad_y": 16, "radius": 0, "geometry": "libass_bs3" }, "letter_spacing": 1, "align": 7 } },
  "parts": [
    { "id": "01-payoff-banana", "out": [0, 55],
      "picture": { "source": "mkv1440", "first_frame": 79612 },
      "audio": { "source": "proxy", "first_sample": 130006400, "rate": 48000, "gain_db": 0,
                 "fade_in": { "samples": 144, "shape": "linear" }, "fade_out": { "samples": 144, "shape": "linear" } },
      "native": { "frames": [81254, 81309], "fps": { "num": 30, "den": 1 } },
      "layers": [ { "id": "screen", "role": "screen", "crop": { "x": 1222, "y": 392, "w": 418, "h": 688 }, "place": { "x": 250, "y": 410, "w": 580, "h": 954 }, "z": 0, "time_map": { "mode": "live" } },
                  { "id": "cam", "role": "cam", "crop": { "x": 200, "y": 1350, "w": 32, "h": 20 }, "place": { "x": 0, "y": 1900, "w": 32, "h": 20 }, "z": 1, "visible": false } ] },
    { "id": "02-claim", "out": [55, 176],
      "picture": { "source": "mkv1440", "first_frame": 79825 },
      "audio": { "source": "proxy", "first_sample": 130347200, "rate": 48000, "gain_db": 0,
                 "fade_in": { "samples": 144, "shape": "linear" }, "fade_out": { "samples": 144, "shape": "linear" } },
      "native": { "frames": [81467, 81588], "fps": { "num": 30, "den": 1 } },
      "layers": [ { "id": "screen", "role": "screen", "crop": { "x": 770, "y": 220, "w": 1280, "h": 900 }, "place": { "x": 160, "y": 420, "w": 760, "h": 534 }, "z": 0, "time_map": { "mode": "live" } },
                  { "id": "cam", "role": "cam", "crop": { "x": 2060, "y": 634, "w": 496, "h": 340 }, "place": { "x": 160, "y": 1120, "w": 760, "h": 520 }, "z": 1 } ] },
    { "id": "03-terrible-prompt", "out": [176, 397],
      "picture": { "source": "mkv1440", "first_frame": 75956 },
      "audio": { "source": "proxy", "first_sample": 124156800, "rate": 48000, "gain_db": 0,
                 "fade_in": { "samples": 144, "shape": "linear" }, "fade_out": { "samples": 144, "shape": "linear" } },
      "native": { "frames": [77598, 77819], "fps": { "num": 30, "den": 1 } },
      "layers": [ { "id": "screen", "role": "screen", "crop": { "x": 1040, "y": 150, "w": 1520, "h": 760 }, "place": { "x": 30, "y": 250, "w": 1020, "h": 510 }, "z": 0, "time_map": { "mode": "live" } },
                  { "id": "cam", "role": "cam", "crop": { "x": 2060, "y": 634, "w": 496, "h": 340 }, "place": { "x": 160, "y": 1120, "w": 760, "h": 520 }, "z": 1 },
                  { "id": "detail-0", "role": "detail", "crop": { "x": 1074, "y": 250, "w": 425, "h": 35 }, "place": { "x": 30, "y": 800, "w": 1020, "h": 84 }, "z": 2, "time_map": { "mode": "live" } } ] } ],
  "text_events": [
    { "id": "hook", "role": "text_hook", "out": [0, 177], "layer": 2, "style": "Hook", "size": 88, "pos": { "x": 540, "y": 305 }, "align": 5,
      "runs": [ { "text": "My PC made this\nin " }, { "text": "67 seconds.", "fill": "#FFD400FF" } ], "provenance": { "kind": "import", "detail": { "file": "captions-v4.ass", "key": "Dialogue 1" } } },
    { "id": "pill-1", "role": "section_pill", "out": [177, 397], "layer": 3, "style": "Pill", "size": 50, "pos": { "x": 48, "y": 262 }, "align": 7,
      "runs": [ { "text": "MiniMax H3  " }, { "text": "running on my PC", "fill": "#FFD400FF" } ] },
    { "id": "c01", "role": "caption", "out": [55, 67],   "layer": 1, "style": "Speech", "size": 86, "pos": { "x": 540, "y": 1065 }, "runs": [ { "text": "we were able" } ] },
    { "id": "c02", "role": "caption", "out": [67, 87],   "layer": 1, "style": "Speech", "size": 86, "pos": { "x": 540, "y": 1065 }, "runs": [ { "text": "to generate that" } ] },
    { "id": "c03", "role": "caption", "out": [87, 111],  "layer": 1, "style": "Speech", "size": 86, "pos": { "x": 540, "y": 1065 }, "runs": [ { "text": "five-second video" } ] },
    { "id": "c04", "role": "caption", "out": [111, 147], "layer": 1, "style": "Speech", "size": 86, "pos": { "x": 540, "y": 1065 }, "runs": [ { "text": "in less than" } ] },
    { "id": "c05", "role": "caption", "out": [147, 177], "layer": 1, "style": "Speech", "size": 86, "pos": { "x": 540, "y": 1065 },
      "runs": [ { "text": "two", "fill": "#FFD400FF" }, { "text": " " }, { "text": "minutes.", "fill": "#FFD400FF" } ] },
    { "id": "c06", "role": "caption", "out": [186, 202], "layer": 1, "style": "Speech", "size": 86, "pos": { "x": 540, "y": 1065 }, "runs": [ { "text": "I mean this" } ] },
    { "id": "c07", "role": "caption", "out": [202, 214], "layer": 1, "style": "Speech", "size": 86, "pos": { "x": 540, "y": 1065 }, "runs": [ { "text": "is gonna be" } ] },
    { "id": "c08", "role": "caption", "out": [214, 243], "layer": 1, "style": "Speech", "size": 86, "pos": { "x": 540, "y": 1065 }, "runs": [ { "text": "a terrible prompt," } ] },
    { "id": "c09", "role": "caption", "out": [243, 258], "layer": 1, "style": "Speech", "size": 86, "pos": { "x": 540, "y": 1065 }, "runs": [ { "text": "but let's" } ] },
    { "id": "c10", "role": "caption", "out": [258, 270], "layer": 1, "style": "Speech", "size": 86, "pos": { "x": 540, "y": 1065 }, "runs": [ { "text": "just say" } ] },
    { "id": "c11", "role": "caption", "out": [270, 348], "layer": 1, "style": "Speech", "size": 86, "pos": { "x": 540, "y": 1065 }, "runs": [ { "text": "a cartoon banana" } ] },
    { "id": "c12", "role": "caption", "out": [349, 397], "layer": 1, "style": "Speech", "size": 86, "pos": { "x": 540, "y": 1065 }, "runs": [ { "text": "peeling itself." } ] } ],
  "audio": { "envelopes": [ { "id": "banana-voice-payoff", "out_samples": [1600, 86400], "gain_db": 10, "ramp_in_samples": 3840, "ramp_out_samples": 5760, "curve": "linear_db",
                              "provenance": { "kind": "import", "detail": { "file": "render-spec-v4.json", "key": "program_gain_envelopes[0]" } } } ],
             "loudness": { "mode": "static", "integrated_lufs": -16, "true_peak_dbtp": -1.5 } },
  "checks": { "safe_zones": "instagram" } }
```

How the example's numbers were derived:
- **Native frames.** From the receipt (`segments[].native_frames`). Part 1's start is RT's `int(2708.45 × 30 + 0.5)`; `2708.45 × 30 = 81253.5` is a tie, so 81254 (M0-03).
- **Picture index.** `native − 1642` (all three parts start after ~1000 s on the stream clock, `rt-sources.toml`): 81254 → 79612, 81467 → 79825, 77598 → 75956.
- **Audio sample.** `native × 1600` on the proxy's clock: 130 006 400, 130 347 200, 124 156 800. Part lengths 55 / 121 / 221 frames = 88 000 / 193 600 / 353 600 samples, equal to the receipt's `audio_samples`.
- **Fades.** Both joins are discontinuous (81309 ≠ 81467; 81588 ≠ 77598), so every fade is allowed by rule 7.
- **Text event frames.** `out = [F(start_cs), F(end_cs))` from the ASS, with `F` the display rule of §2.4 (here equal to `ceil(0.3·cs)`): hook 0–587 cs → [0, 177); pill 587 cs → 177; "we were able" 183–223 cs → [55, 67); "peeling itself." 1163–1323 cs → [349, 397).
- **Layers.** Crops and places from the receipt; aspect differences 0.07 %, 0.07 %, 0.00 %, cam 0.19 %, detail 0.00 % (all ≤ 0.5 %). Part 1's cam is RT's dummy crop (≤ 32×20 at y ≥ 1880) and is hidden.
- **Envelope.** `program_gain_envelopes[0]`: output samples [1600, 86400), +10 dB, ramps 80/120 ms = 3840/5760 samples.

### 2.4 Field-by-field mapping (RT → RenderPlan v0)

**Rounding.** RT uses two rules, and the converter reproduces each where RT used it, on the same IEEE-754 double product `t × 30`; it never re-derives a frame from seconds any other way (SPEC §4.1.1):
- **Part edges**: `int(t·fps + 0.5)` (ties up), from `render_reel_rt.py` and `story_reel.py` `layout()`. The receipt's `native_frames` win when present.
- **Hold frames, gain-envelope edges, `hook.until_s`**: Python `round(t·fps)`, which rounds ties to **even** (`rt.frame`, `story_reel.at`, `story_reel.write_ass`). In Rust this is `(t * 30.0).round_ties_even()`, never `f64::round` (ties away from zero). Example ties: `104.35 × 30 = 3130.5` → 3130 (half-even) vs 3131 (`int(+0.5)`); `126.15 × 30 = 3784.5` → 3784 vs 3785.
- **ASS event times**: RT writes centiseconds `cs(f) = min(round(f·100/30), floor(total·100/30))` (story_reel as committed, which wrote every golden v5 ASS; a later working copy uses `floor`, and `--cues build` picks whichever reproduces the cut's own ASS). The displayed frames are `[F(start_cs), F(end_cs))`, where `F(cs)` is the first output frame `f` whose millisecond as ffmpeg's `ass` filter computes it, `(long long)(f · (1.0/30) · 1000)` in doubles, is ≥ `10·cs`. This truncation makes `F` one frame later than `ceil(0.3·cs)` on 20 event edges of the golden cuts (none in §2.3), e.g. 13.30 s → frame 400 (see the ASS table).

**Fit.** RT shrinks text with its own measure, and the converter writes the result into `text_events[].size`; nothing downstream refits (rule 6). RT's `fit_size(text, size, max_px)`: `w = max over lines of Pillow ImageFont.truetype(font, size).getlength(line) × 0.838` (`LIBASS_PER_PILLOW`), then `size if w ≤ max_px else int(size · max_px / w)`. RT applies it to hooks (`hook_size`, default 88, at 1000 px; `\fs` written only when it differs), captions (86 at 1000 px) and pills (50 at 950 px; `\fs` always written). The Rust converter reproduces Pillow's advance width (FreeType advances at `size` px with Pillow's basic layout) and is checked against a `tools/` table of Pillow widths for every golden hook, cue and pill; with `--cues ass` the ASS `\fs` (or the style size) is copied verbatim.

**From `plan.json` (story_reel input).** "Used" counts are out of the 10 reel plans (h79 has no plan.json):

| RT key | used | RenderPlan v0 | rule |
|---|---|---|---|
| `hook_id`, `title`, `content_id`, `version` | 10/10/6/10 | `meta.*` | passthrough |
| `merges`, `merged_content_ids` | 2/1 | `meta.covers` | passthrough (M0-26) |
| `vid` | 10 | `sources[]` | resolve via `rt-sources.toml` (mode `native` → one source; `hires_mkv` → picture MKV + audio proxy, piecewise offsets from `alignment.json`) |
| `hook.text` | 10 | `text_events[role=text_hook].runs` | `{Y}`→`#FFD400FF`, `{G}`→`#3DDC84FF`, `{W}`→`#FFFFFFFF`; `\N`→`\n` |
| `hook.until` (cut id) | 9 | `.out[1]` | that part's output end frame `e`, then the ASS rule: `F(cs(e))` |
| `hook.until_s` | 1 | `.out[1]` | `e = round_half_even(until_s·30)`, then `F(cs(e))` |
| `hook.y` | 1 (default 305) | `.pos.y`; `.pos.x = 540`, `align 5` | — |
| `hook_size` | 0 (default 88) | `text_styles.Hook.size`; event `.size` | event size = `fit_size(text, hook_size, 1000)` (Fit, above; M0-16) |
| `layouts.<n>.screen_crop` / `screen_place` | 37 | layer `role=screen` | px → `crop`/`place`; aspect checked ≤ 0.5 % |
| `layouts.<n>.camera_crop` / `camera_place` | 37 | layer `role=cam` | dummy (place ≤ 32×20, y ≥ 1880) → `visible:false` (M0-18) |
| `layouts.<n>.detail_layers[]{crop,place}` | 8 layouts | layers `role=detail`, z = 2+i | — |
| `caption_y.<layout>` | 7 | caption events' `pos.y` by the layout of the part the cue starts in | default 1065 |
| `cuts[].id` | 101 | `parts[].id` | stable |
| `cuts[].a`, `.z` | 101 | `parts[].out`, `picture.first_frame`, `audio.first_sample` | **receipt `native_frames` authoritative**; else `int(t·30 + 0.5)` (M0-03) |
| `cuts[].layout` | 101 | `parts[].layers` | resolved copy |
| `cuts[].pill` → `pills.<n>.text` | 85 / 32 | `text_events[role=section_pill]` | starts at max(part start, hook end frame `e`); merged across consecutive parts with identical text; size `fit_size(text, 50, 950)` |
| `pills.<n>.clock_from`, `.label` | 2 | `text_events[role=run_clock]` one per displayed second | text `label + "  " + {Y}m:ss` of `int(native_frame/30 − clock_from)` (floor, clamped at 0); provenance import (M0-21) |
| `cuts[].hold_at` | 7 | `time_map:{mode:hold, frame}` on screen + detail layers | native: first PTS ≥ t (`ffmpeg -ss t`); hires: `round_half_even(t·30) + offset(t)`; validated against `holds/*.png` (M0-17) |
| `cuts[].still` | 1 | `time_map:{mode:freeze_first}` on the screen layer; detail layers stay `live` (RT loops the screen branch only) | — |
| `cuts[].gain_db` | 0 | `parts[].audio.gain_db` | — |
| `gain[]{a,z,db}` | 1 | `audio.envelopes[]` | `at(t) = c0 + round_half_even(t·30) − q0` (RT `story_reel.at`); `out_samples = [at(a)·1600, at(z)·1600]`; ramps 80/120 ms = 3840/5760 samples |
| `keywords`, `keep`, `fix`, `cap_sub`, `no_caption` | 10/10/9/10/3 | consumed by the cue builder (B-08) only | with `--cues ass` they are already applied in the ASS |
| `words: "stream"` | 0 | — | `Unsupported` in v0 |

**From `render-spec-*.json`.** Used counts are out of 11 specs and 115 segments:

| key | used | RenderPlan v0 |
|---|---|---|
| `fps`, `canvas` | 11 | `output.fps` (`{num, den: 1}`), `output.canvas` |
| `background` `0xRRGGBB` | 11 | `background{kind:colour}` |
| `source`, `source_video_id`, `segments[].source`, `source_native_origin` | 11 / 115 | used only to identify the window → original mapping; never rendered from |
| `segments[].native_start/end` | 115 | cross-check only (the receipt wins) |
| `segments[].audio_gain_db` | 115 (all 0) | `parts[].audio.gain_db` |
| `segments[].screen_*`, `camera_*`, `detail_layers` | 115 | layers (as above) |
| `segments[].screen_hold` (PNG), `screen_hold_native_time` | 7 | `time_map.hold.frame` (rounding as `hold_at`); the PNG is a test oracle only |
| `segments[].screen_still` | 1 | `freeze_first` on the screen layer only |
| `captions_ass` | 11 | `text_events` + `text_styles` via the ASS importer (B-07) |
| `program_gain_envelopes[]{output_frames, gain_db, ramp_in_ms, ramp_out_ms, curve}` | 2 specs, 3 envelopes | `audio.envelopes[]` (frames ×1600; ms × 48 → samples) |
| `opening_overlay`, `supporting_video`, `closing_screen_hold`, `camera_rgba*`, `detail_crop` | 0 | `Unsupported` in v0, listed by name (§2.6) |
| `plan`, `note`, `hook_id`, `content_id`, `title` | 11 | `meta` |

**From `render-receipt.json`** (authoritative numbers):

| key | RenderPlan v0 |
|---|---|
| `segments[].native_frames`, `frames`, `n`, `output_frames` | `parts[].native.frames`, `parts[].out`, and for MKV parts `first_frame = native_frames[0] + offset(t)` |
| `segments[].fade_in` / `fade_out` | `parts[].audio.fade_in/out = {144, linear}` or null; must satisfy rule 7 (B-14 checks it) |
| `segments[].audio_samples` | cross-check: equals `S(out[1]) − S(out[0])` |
| `output.frames`, `output.sha256`, output path | `output.frames`, `checks.reference` |

**From the ASS** (11 files; `[Script Info] PlayResX 1080, PlayResY 1920, WrapStyle 2, ScaledBorderAndShadow yes`):

| ASS | RenderPlan v0 |
|---|---|
| `Style: Speech,Neometric Heavy,86,&H00FFFFFF,…,&H00000000,&H00000000,-1,…,0,1,12,7,5,…` | `text_styles.Speech {font, size 86, fill #FFFFFFFF, outline{12, #000000FF, round}, shadow{7,7,blur 0, #000000FF, of fill_and_outline}, align 5}` |
| `Style: Hook,…,88,…,1,12,7,5` | `text_styles.Hook` (same, size 88) |
| `Style: Pill,…,50,&H00FFFFFF,&H00FFFFFF,&H30101010,…,Spacing 1,…,3,16,0,7` | `text_styles.Pill {size 50, box{#101010CF, pad 16/16, radius 0, libass_bs3}, letter_spacing 1, align 7}` |
| `Fontname` | one `fonts[]` entry per distinct face, `em_per_size` from the font file (S4) |
| `Dialogue: L,start,end,Style,…,text` | `text_events[]`. `layer = L`. `out = [F(start_cs), F(end_cs))` (the display rule under "Rounding": libass on ffmpeg's truncated millisecond frame times, which is what shipped). `size` = the event's `\fs`, else the style's `Fontsize` |
| `{\pos(x,y)}`, `{\an7}`, `{\fsN}`, `{\c&HBBGGRR&}`, `\N` | `pos`, `align`, `size`, run `fill` (BGR→RGB, alpha FF), `\n` |

### 2.5 ffmpeg features in the RT path, and the gmclip M0 ticket that replaces each

| RT / ffmpeg | gmclip M0 | ticket |
|---|---|---|
| `-ss (k0−0.5)/fps` + `trim=end_frame=n` + `setpts=N` | presentation-index addressing on own index + VT decode + reorder | B-01, B-02, H-02, H-03 |
| window builder (x264 crf 12 MOV + `pcm_s24le` proxy audio, MAE check ±1 frame) | **removed**: the converter addresses originals; the MAE check becomes a converter test against hold PNGs and sample frames | B-13, B-14 |
| `crop=…:exact=1` on yuv420p | sub-pixel crop sampled from NV12 planes | H-05 |
| `scale=…:flags=lanczos` (swscale Lanczos, a=3, chroma scaled separately) | separable Lanczos-3 in RGBA16F after YUV→R'G'B' (chroma upsampling difference accepted; measured in H-05) | H-05 |
| `color=c=0x0c1118`, `overlay=x:y:format=yuv444` | solid background, premultiplied over at (sub)pixel positions | H-05 |
| `loop` first frame (`screen_still`), `-loop 1` PNG (`screen_hold`) + `scale=out_color_matrix=bt709` | `time_map` freeze_first / hold on decoded source frames (no PNG, no matrix round trip) | H-05, H-10 |
| `concat` (µs timestamps, then frame clock restored) | part tiling on the integer output-frame clock | H-10 |
| `ass` (libass, Neometric Heavy, BS1/BS3, `\pos \an \c \fs \N`) | parley + vello_cpu cue raster at the plan's explicit sizes, cached tiles, composited per frame | B-07, B-09, B-10, H-06 |
| Pillow `getlength × 0.838` fit (`story_reel.fit_size`) | resolved by the converter into `text_events[].size` | B-08, B-13 |
| `aresample=48000`, `aformat=s32`, `atrim`/`apad` sample-exact | AudioToolbox decode (edit list/priming honoured) + sample-exact slicing | H-04, B-11 |
| `volume=…dB`, `afade ns=144` at discontinuous joins | part gain, linear fades at discontinuous joins only (rule 7) | B-11 |
| `aeval` linear-dB ramps over sample index | envelopes, sample-exact | B-11 |
| `loudnorm=I=-16:TP=-1.5:LRA=11` (dynamic) | static gain + 4× oversampled TP limiter (replacement) | B-12 |
| `libx264 medium crf 17`, `-r 30 -vsync 1`, yuv420p | VT H.264 High@4.0, B=2, closed GOP 1 s, VBR 16/24 Mbps from NV12 render targets | H-07, H-08 |
| `aac -b:a 192k` | AudioToolbox AAC-LC 192k, priming D | H-09 |
| `-movflags +faststart` | `Mp4Writer` moov-first, no `edts` | B-04, B-05 |
| `ffprobe` verify (codecs, size, SAR, fps, frame count, durations, clean decode) | own verify (re-demux + OS decode + xcorr per join + ebur128) | B-15 |
| QA: `ebur128`, contact sheet with IG zones, ASR round trip | M0: `gmclip still` sheet + dev-only `tools/join_asr`; M1: `see`, `verify{joins}` | H-05b, B-15 |

### 2.6 Not expressible in v0 (needs `gmclip.renderplan/1`)

The converter reports each as `Unsupported{reason}` by name; no golden cut uses any of them.
- **Image overlays** (PNG/JPEG/WebP/SVG), video and shape overlays. Image overlays are [M2] in SPEC §5.9, with decoding per R-MEDIA-SEC-3 / R-SVG-1.
- **Text below a layer** (SPEC overlay `z` interleaved with layers). v0 draws every text event above every layer.
- **Crossfades** between parts (SPEC §5.6 Crossfades [M2]).
- **NTSC and other non-integer output rates** (rule 4).
- Key, feathers, mirror, grade, plate-as-backdrop video, cover layers, roto, keyframes, caption animation (word bounce), and per-layer opacity ramps.
- RT `opening_overlay`, `supporting_video`, `closing_screen_hold`, `camera_rgba*`, `detail_crop`, and plans with `words: "stream"`.
- TikTok, Shorts and YouTube safe zones in `checks.safe_zones` (measured in S3-mac; added in `/1`).

### 2.7 Validation record (2026-10-14)

Method: the `json` blocks in §2.2 and §2.3 were extracted from this file and checked with python `jsonschema` 4.25.0 (`Draft202012Validator`). Rule 10 (referential integrity), rule 2 (tiling), rule 4 (sample counts) and rule 7 (fades) were checked by a short script implementing those rules, standing in for the W0-07 validator.

| Check | Result |
|---|---|
| Schema parses as JSON | pass |
| `Draft202012Validator.check_schema` (2020-12 metaschema) | pass |
| Every object schema in the document has `additionalProperties: false`, or is a map with `propertyNames` and a closed value schema | pass: 0 open object schemas found by a walk of the whole schema |
| §2.3 example validates against the schema | pass, 0 errors |
| §2.3 example satisfies rules 2, 4, 5 (aspect), 7, 10 | pass, 0 violations |

Probes (mutations of the example; every row but the last must be rejected):

| Mutation | Result | Reason reported |
|---|---|---|
| layer `"visable": false` (typo inside a layer) | **REJECT** (schema) | Additional properties are not allowed ('visable' was unexpected) |
| `meta.typo`, `provenance.detail.anything` | REJECT (schema) | additional property |
| `text_style {max_width, fit}` / `{line_height}` | REJECT (schema) | additional property (fit is resolved before the plan, rule 6) |
| `part.audio.crossfade` | REJECT (schema) | additional property (crossfades are not in v0, §2.6) |
| `time_map {mode: hold, frame, at_src}` | REJECT (schema) | not valid under any `oneOf` branch |
| `background {kind: colour, rgb, stops}` | REJECT (schema) | not valid under any `oneOf` branch |
| `output.fps 30000/1001` | REJECT (schema) | 30000 is not one of [24, 25, 30, 48, 50, 60] |
| top-level `image_events` | REJECT (schema) | additional property (image overlays are M2, §2.6) |
| text events present, `fonts` removed | REJECT (schema) | 'fonts' is a dependency of 'text_events' |
| `fonts[].em_per_size` missing; text event without `size` | REJECT (schema) | required property |
| `text_events[].style: "Nope"` | REJECT (rule 10) | style undefined |
| `text_styles.Hook.font: "nope"`; `parts[1].picture.source: "nope"` | REJECT (rule 10) | id undefined |
| duplicate text event id | REJECT (rule 10) | duplicate id |
| parts leave a gap (`[56, 176]`); `output.frames 1807` with 397 frames of parts (the v4 abbreviated example) | REJECT (rule 2) | parts do not tile `[0, frames)` |
| text event past `output.frames` | REJECT (rule 2) | outside `[0, frames)` |
| fade on a continuous join (part 2 starting at part 1's end sample) | REJECT (rule 7) | fade at continuous join |
| `layer.role: "chat"`, `provenance.kind: "ocr"` | ACCEPT (intended: SPEC vocabularies, COMP #27) | — |

Re-run with `python3 -I validate_rp.py spec/renderplan-v0.md` (`critique/round-4/validate_rp.py`); W0-07's Rust validator must reproduce every row.
