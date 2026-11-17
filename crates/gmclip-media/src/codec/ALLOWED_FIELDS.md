# codec/ALLOWED_FIELDS.md

**Owner: AJ (code-owner review, SPEC §15.0, D3).** This is the complete list of what
`gmclip-media/src/codec/` reads from coded bitstreams. Nothing here decodes or encodes
picture or audio data; no slice, tile, macroblock or residual syntax is parsed. A field
not listed here may not be read; adding one is a change to this file that AJ approves.

Each field names the SPEC §15.0 clause it serves:
**[reorder]** reorder depth · **[profile]** profile/level · **[chroma]** chroma format ·
**[depth]** bit depth · **[colour]** colour description · **[size]** frame sizes ·
**[superframe]** VP9 superframe index/split · **[vpcC]** `vpcC` synthesis ·
**[show]** VP9 show_existing_frame. ("frame sizes" is a VP9 clause; H.264 picture size
is read only for the DPB bound behind the reorder depth.)

"Skipped" fields are read only to advance past them to a listed field, and their values
are discarded. Where the OS parses the same value (VideoToolbox format descriptions),
the OS value is preferred at decode time; these readers serve indexing, planning,
`vpcC` and checks.

## H.264 (`codec/h264.rs`; ITU-T H.264 (08/2021))

Container record, not bitstream: `avcC` (ISO/IEC 14496-15 §5.3.3.1) — configurationVersion,
AVCProfileIndication, profile_compatibility, AVCLevelIndication, lengthSizeMinusOne, the
SPS NAL units (as bytes) and the PPS count. PPS contents are not read.

NAL header of an SPS (§7.3.1): forbidden_zero_bit, nal_ref_idc (skipped), nal_unit_type
(must be 7). Emulation-prevention bytes are removed first (§7.4.1).

Sequence parameter set (§7.3.2.1.1):

| Field | Kept / skipped | Clause |
|---|---|---|
| profile_idc, constraint_set0–5 flags, level_idc | kept | [profile] |
| seq_parameter_set_id | kept | — (names the SPS the kept fields belong to) |
| chroma_format_idc, separate_colour_plane_flag | kept / skipped | [chroma] |
| bit_depth_luma_minus8, bit_depth_chroma_minus8 | kept | [depth] |
| qpprime_y_zero_transform_bypass_flag | skipped | — |
| seq_scaling_matrix_present_flag, seq_scaling_list_present_flag, scaling lists (delta_scale) | skipped | — |
| pic_order_cnt_type | kept | [reorder] (type 2: output order is decode order) |
| log2_max_frame_num_minus4, the pic_order_cnt_type fields (log2_max_pic_order_cnt_lsb_minus4, delta_pic_order_always_zero_flag, offset_for_non_ref_pic, offset_for_top_to_bottom_field, num_ref_frames_in_pic_order_cnt_cycle, offset_for_ref_frame[]) | skipped | — |
| max_num_ref_frames, gaps_in_frame_num_value_allowed_flag | skipped | — |
| pic_width_in_mbs_minus1, pic_height_in_map_units_minus1, frame_mbs_only_flag | kept | [reorder] (MaxDpbFrames when the VUI has no bitstream restriction) |
| mb_adaptive_frame_field_flag, direct_8x8_inference_flag | skipped | — |
| frame_cropping_flag, frame_crop_left/right/top/bottom_offset | skipped (picture size comes from the container and the OS) | — |
| vui_parameters_present_flag | kept | — |

VUI (Annex E.1.1):

| Field | Kept / skipped | Clause |
|---|---|---|
| aspect_ratio_info_present_flag, aspect_ratio_idc, sar_width, sar_height | skipped | — |
| overscan_info_present_flag, overscan_appropriate_flag | skipped | — |
| video_signal_type_present_flag, video_format | skipped | — |
| video_full_range_flag | kept | [colour] |
| colour_description_present_flag, colour_primaries, transfer_characteristics, matrix_coefficients | kept | [colour] |
| chroma_loc_info_present_flag, chroma_sample_loc_type_top_field/bottom_field | skipped (siting comes from the container and the OS) | — |
| timing_info_present_flag, num_units_in_tick, time_scale, fixed_frame_rate_flag | skipped | — |
| nal/vcl_hrd_parameters_present_flag and hrd_parameters() (cpb_cnt_minus1, bit_rate_scale, cpb_size_scale, bit_rate_value_minus1, cpb_size_value_minus1, cbr_flag, the four length/delay fields), low_delay_hrd_flag, pic_struct_present_flag | skipped | — |
| bitstream_restriction_flag, motion_vectors_over_pic_boundaries_flag, max_bytes_per_pic_denom, max_bits_per_mb_denom, log2_max_mv_length_horizontal/vertical | skipped | — |
| max_num_reorder_frames, max_dec_frame_buffering | kept | [reorder] |

Reorder depth: `max_num_reorder_frames` when the VUI has a bitstream restriction.
Otherwise, when the container has a sample table, the depth the composition offsets
measure (max over frames of decode index − presentation index: container data, not
codec data; this is how VUI-silent streams such as (a) and the proxies match ffprobe's
`has_b_frames`). Otherwise the H.264 E.2.1 inference: 0 for the intra profiles (44, and
86/100/110/122/244 with constraint_set3) and for pic_order_cnt_type 2, else MaxDpbFrames
from the level (Table A-1, level 1b included, ≤ 16). A VUI that fails to parse is
dropped and the SPS fields before it are kept.

## VP9 (`codec/vp9.rs`; VP9 Bitstream & Decoding Process Specification v0.6)

Superframe index (Annex B): the trailing marker byte (superframe_marker, bytes_per_framesize_minus_1,
frames_in_superframe_minus_1), the repeated leading marker, and frame_sizes[]. Used to
split a packet into frames — **[superframe]**.

Uncompressed header (§6.2), read in order and stopped after the frame size:

| Field | Kept / skipped | Clause |
|---|---|---|
| frame_marker, profile_low_bit, profile_high_bit, reserved_zero (profile 3) | kept | [profile] |
| show_existing_frame, frame_to_show_map_idx | kept | [show] |
| frame_type, show_frame | kept | [show], [size] (which frames carry a colour config and size) |
| error_resilient_mode | read, value discarded | — (says whether reset_frame_context follows) |
| intra_only, reset_frame_context | kept / skipped | [size] (intra-only frames carry a size) |
| frame_sync_code | checked | — |
| color_config(): ten_or_twelve_bit, color_space, color_range, subsampling_x, subsampling_y, reserved_zero | kept | [depth], [colour], [chroma] |
| refresh_frame_flags (intra-only frames) | skipped | — |
| frame_size(): frame_width_minus_1, frame_height_minus_1 | kept | [size] |
| render_size(): render_and_frame_size_different, render_width_minus_1, render_height_minus_1 | kept | [size] |

Inter frames: parsing stops after `reset_frame_context` (their size comes from a
reference). Nothing after the frame size is read (no loop filter, quantisation,
segmentation, tile info, compressed header or tile data).

`vpcC` synthesis (VP Codec ISO Media File Format Binding v1.0 §2.3) **[vpcC]**: version 1,
flags 0; profile; level from luma picture size and luma sample rate (WebM VP9 levels
table; the container frame rate); bitDepth; chromaSubsampling (4:2:0 → 1, or 0 when the
container says chroma is sited left; 4:2:2 → 2; 4:4:4 → 3); videoFullRangeFlag
(container, else color_range); colourPrimaries and transferCharacteristics from the
container (else 2, unspecified); matrixCoefficients from the container, else from
color_space; codecInitializationDataSize 0.

## Allowed by SPEC §15.0, not read yet

HEVC SPS/VPS VUI fields, NAL unit types (beyond the SPS type check above), AV1 OBU headers
and the sequence header OBU (`av1C` synthesis), ADTS headers. Each gets rows here, with
its clause, before code reads it.
