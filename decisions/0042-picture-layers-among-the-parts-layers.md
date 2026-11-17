# 0042 — Picture layers: added videos and images among the part's layers, roto on added videos

Date: 2026-10-19 · Decided by: agent (AJ: "multiple layers of video / images on the timeline including possible alpha layers / rotoscoping"; AJ chose steps 1–3, transparent video later) · Status: adopted, AJ may veto

## Context
Added videos (clip events, decision 0028) and images (image events, decision 0014) always drew above every layer of the part. Nothing could sit behind a cut-out speaker (decision 0013), and an added video could not be cut out itself. The timeline drew every overlay as an unlabelled text bar, and a file dropped on the window joined the project as another source, never as a layer at a time.

## Decision
- **RenderPlan `/1`, rule 12 (optional fields, so plans without them are byte-identical):**
  - `clip_events[].z` and `image_events[].z` place the event in the part layers' `z` space. It is drawn just under the first drawn part layer whose `z` is at least its own, or above them all when there is none. Events in the same place are ordered by `(z, layer, clip before image, array order)`.
  - Without `z`, an event keeps its old place.
  - `clip_events[].matte` is the `$defs/matte` of a part layer. Its track spans the source frames the clip shows (`retime::clip_span`), and it shares the matte cache key with any part layer of the same source, span and crop.
- **Draw list:**
  - `FrameDraw.tiles_at` lists the image events drawn among the layers: `(before layers[i], id)`.
  - `TextLayer::active_split` returns those tiles apart from the ones above, from one tile cache.
  - The CPU (`compose_with`) and the GPU (`set_among`) draw them between the layer passes.
- **Model:**
  - `Overlay.under: Option<i32>` sets that `z` for an image or video overlay.
  - `OverlayVideo.matte: Option<Matte>` holds the cut-out, roto only.
- **Commands:**
  - `overlay_stack_set {overlay, under}`.
  - `matte_set` takes `{overlay}` in place of `{layer}`. An overlay's cut-out has no per-part override and no brush strokes yet.
  - The R.01 background build includes matted overlays.
- **Timeline:**
  - Video and image overlays get their own lane colours. An edge along the foot marks one that sits behind the picture, and the label says "Video · cut out · behind".
  - A video or picture dropped on the timeline becomes an overlay at the frame under the drop. `HomeEvent::Dropped.at` carries the drop point. Elsewhere, a drop behaves as before.
- **Inspector:** each image or video overlay has a layer-order menu: in front, behind the whole picture, or behind each picture layer by role. Video overlays also have a "Cut out person" switch.

## Evidence
- `crates/gmclip-plan/tests/v1.rs::events_among_the_layers`
- `crates/gmclip-render/tests/backdrop_clips.rs`:
  - `a_clip_with_z_is_drawn_among_the_layers`
  - `an_image_with_z_is_drawn_under_a_layer`
- `crates/gmclip-render/tests/gpu_compose.rs::gpu_tiles_among_the_layers_match_the_cpu` (Metal): max |GPU − CPU| is 1 code, against the gate of 2. This includes the "nothing visible" path.
- `app/ui/tests/e2e/layers.spec.ts`: drop on the timeline, put the overlay behind, cut out, undo.
- `gmclip still` on `own-vm-1080.mp4`, an added video in front of, behind, and cut out over a screen layer: the export path draws each as described.

## Sections changed
- `schemas/renderplan@1.json`:
  - `image_event.z`
  - `clip_event.z`
  - `clip_event.matte`
- Not in this decision: video with its own alpha (ProRes 4444, HEVC with alpha, WebM alpha). It needs the HEVC/ProRes decode SPEC lists at [M2].
