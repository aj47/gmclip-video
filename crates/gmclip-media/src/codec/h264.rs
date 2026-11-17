//! H.264 allowed fields (SPEC §15.0, `codec/ALLOWED_FIELDS.md`): the `avcC` record's
//! structure and, inside each sequence parameter set, profile, level, chroma format,
//! bit depth, colour description and the reorder depth (VUI, or the level's DPB size). Everything
//! else in the SPS is read only to reach the VUI and is not kept. No slice data, no
//! PPS contents, no other NAL unit is parsed.

use super::bits::BitReader;
use super::{CodecError, Result};

/// The `avcC` decoder configuration record (ISO/IEC 14496-15 §5.3.3.1), structure only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvcConfig {
    pub profile: u8,
    pub compatibility: u8,
    pub level: u8,
    /// Bytes in each sample's NAL length prefix (1, 2 or 4).
    pub nal_length_size: u8,
    /// Sequence parameter set NAL units, header byte included, escaped as stored.
    pub sps: Vec<Vec<u8>>,
    /// Number of picture parameter sets (contents not read).
    pub pps_count: usize,
}

/// Parse the `avcC` record.
pub fn parse_avcc(data: &[u8]) -> Result<AvcConfig> {
    let get = |i: usize, f: &'static str| {
        data.get(i).copied().ok_or(CodecError::Truncated {
            field: f,
            bit: i * 8,
        })
    };
    if get(0, "configurationVersion")? != 1 {
        return Err(CodecError::Invalid {
            field: "configurationVersion",
            reason: "not 1",
        });
    }
    let profile = get(1, "AVCProfileIndication")?;
    let compatibility = get(2, "profile_compatibility")?;
    let level = get(3, "AVCLevelIndication")?;
    let nal_length_size = (get(4, "lengthSizeMinusOne")? & 3) + 1;
    if nal_length_size == 3 {
        return Err(CodecError::Invalid {
            field: "lengthSizeMinusOne",
            reason: "3-byte NAL lengths",
        });
    }
    let n_sps = usize::from(get(5, "numOfSequenceParameterSets")? & 0x1F);
    let mut at = 6usize;
    let mut sps = Vec::with_capacity(n_sps);
    for _ in 0..n_sps {
        sps.push(length_prefixed(data, &mut at, "sequenceParameterSetNALUnit")?.to_vec());
    }
    let pps_count = match data.get(at).copied() {
        Some(n) => {
            at += 1;
            for _ in 0..n {
                length_prefixed(data, &mut at, "pictureParameterSetNALUnit")?;
            }
            usize::from(n)
        }
        None => 0,
    };
    Ok(AvcConfig {
        profile,
        compatibility,
        level,
        nal_length_size,
        sps,
        pps_count,
    })
}

/// A 16-bit length-prefixed unit at `*at`, advancing past it.
fn length_prefixed<'a>(data: &'a [u8], at: &mut usize, f: &'static str) -> Result<&'a [u8]> {
    let short = CodecError::Truncated {
        field: f,
        bit: *at * 8,
    };
    let hi = usize::from(data.get(*at).copied().ok_or(short.clone())?);
    let lo = usize::from(data.get(*at + 1).copied().ok_or(short.clone())?);
    let len = (hi << 8) | lo;
    let unit = data.get(*at + 2..*at + 2 + len).ok_or(short)?;
    *at += 2 + len;
    Ok(unit)
}

/// Remove emulation-prevention bytes (`00 00 03` → `00 00`), H.264 §7.4.1.
pub fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// VUI fields kept (H.264 Annex E.1.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Vui {
    pub video_full_range: Option<bool>,
    /// (colour_primaries, transfer_characteristics, matrix_coefficients)
    pub colour: Option<(u8, u8, u8)>,
    pub max_num_reorder_frames: Option<u32>,
    pub max_dec_frame_buffering: Option<u32>,
}

/// Sequence parameter set fields kept (H.264 §7.3.2.1.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sps {
    pub profile_idc: u8,
    /// constraint_set0..5 flags, MSB first, as the byte stores them.
    pub constraints: u8,
    pub level_idc: u8,
    pub sps_id: u32,
    pub chroma_format_idc: u32,
    pub bit_depth_luma: u32,
    pub bit_depth_chroma: u32,
    pub frame_mbs_only: bool,
    /// Picture size in macroblocks / map units: only for MaxDpbFrames (reorder depth).
    pub width_mbs: u32,
    pub height_map_units: u32,
    /// 2 means output order is decode order (no reordering).
    pub pic_order_cnt_type: u32,
    pub vui: Option<Vui>,
}

const HIGH_PROFILES: &[u8] = &[100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];

fn scaling_list(r: &mut BitReader<'_>, size: usize) -> Result<()> {
    let (mut last, mut next) = (8i32, 8i32);
    for _ in 0..size {
        if next != 0 {
            let delta = r.se("delta_scale")?;
            if !(-128..=127).contains(&delta) {
                return Err(CodecError::Invalid {
                    field: "delta_scale",
                    reason: "out of range",
                });
            }
            next = (last + delta + 256) % 256;
        }
        last = if next == 0 { last } else { next };
    }
    Ok(())
}

fn hrd(r: &mut BitReader<'_>) -> Result<()> {
    let cpb = r.ue_max(31, "cpb_cnt_minus1")?;
    r.skip(8, "bit_rate_scale/cpb_size_scale")?;
    for _ in 0..=cpb {
        r.ue("bit_rate_value_minus1")?;
        r.ue("cpb_size_value_minus1")?;
        r.skip(1, "cbr_flag")?;
    }
    r.skip(20, "hrd delays and lengths")
}

fn vui(r: &mut BitReader<'_>) -> Result<Vui> {
    let mut v = Vui::default();
    if r.flag("aspect_ratio_info_present_flag")? && r.bits(8, "aspect_ratio_idc")? == 255 {
        r.skip(32, "sar_width/sar_height")?;
    }
    if r.flag("overscan_info_present_flag")? {
        r.skip(1, "overscan_appropriate_flag")?;
    }
    if r.flag("video_signal_type_present_flag")? {
        r.skip(3, "video_format")?;
        v.video_full_range = Some(r.flag("video_full_range_flag")?);
        if r.flag("colour_description_present_flag")? {
            v.colour = Some((
                r.bits(8, "colour_primaries")? as u8,
                r.bits(8, "transfer_characteristics")? as u8,
                r.bits(8, "matrix_coefficients")? as u8,
            ));
        }
    }
    if r.flag("chroma_loc_info_present_flag")? {
        r.ue_max(5, "chroma_sample_loc_type_top")?;
        r.ue_max(5, "chroma_sample_loc_type_bottom")?;
    }
    if r.flag("timing_info_present_flag")? {
        r.skip(65, "timing_info")?;
    }
    let nal = r.flag("nal_hrd_parameters_present_flag")?;
    if nal {
        hrd(r)?;
    }
    let vcl = r.flag("vcl_hrd_parameters_present_flag")?;
    if vcl {
        hrd(r)?;
    }
    if nal || vcl {
        r.skip(1, "low_delay_hrd_flag")?;
    }
    r.skip(1, "pic_struct_present_flag")?;
    if r.flag("bitstream_restriction_flag")? {
        r.skip(1, "motion_vectors_over_pic_boundaries_flag")?;
        r.ue("max_bytes_per_pic_denom")?;
        r.ue("max_bits_per_mb_denom")?;
        r.ue("log2_max_mv_length_horizontal")?;
        r.ue("log2_max_mv_length_vertical")?;
        v.max_num_reorder_frames = Some(r.ue_max(16, "max_num_reorder_frames")?);
        v.max_dec_frame_buffering = Some(r.ue_max(16, "max_dec_frame_buffering")?);
    }
    Ok(v)
}

/// Parse one SPS NAL unit (header byte included, escaped as stored).
pub fn parse_sps(nal: &[u8]) -> Result<Sps> {
    let raw = unescape(nal);
    let mut r = BitReader::new(&raw);
    r.skip(1, "forbidden_zero_bit")?;
    r.skip(2, "nal_ref_idc")?;
    if r.bits(5, "nal_unit_type")? != 7 {
        return Err(CodecError::Invalid {
            field: "nal_unit_type",
            reason: "not an SPS (7)",
        });
    }
    let profile_idc = r.bits(8, "profile_idc")? as u8;
    let constraints = r.bits(8, "constraint_set_flags")? as u8;
    let level_idc = r.bits(8, "level_idc")? as u8;
    let sps_id = r.ue_max(31, "seq_parameter_set_id")?;
    let (mut chroma_format_idc, mut bit_depth_luma, mut bit_depth_chroma) = (1, 8, 8);
    if HIGH_PROFILES.contains(&profile_idc) {
        chroma_format_idc = r.ue_max(3, "chroma_format_idc")?;
        if chroma_format_idc == 3 {
            r.skip(1, "separate_colour_plane_flag")?;
        }
        bit_depth_luma = 8 + r.ue_max(6, "bit_depth_luma_minus8")?;
        bit_depth_chroma = 8 + r.ue_max(6, "bit_depth_chroma_minus8")?;
        r.skip(1, "qpprime_y_zero_transform_bypass_flag")?;
        if r.flag("seq_scaling_matrix_present_flag")? {
            let lists = if chroma_format_idc == 3 { 12 } else { 8 };
            for i in 0..lists {
                if r.flag("seq_scaling_list_present_flag")? {
                    scaling_list(&mut r, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    r.ue_max(12, "log2_max_frame_num_minus4")?;
    let pic_order_cnt_type = r.ue_max(2, "pic_order_cnt_type")?;
    match pic_order_cnt_type {
        0 => {
            r.ue_max(12, "log2_max_pic_order_cnt_lsb_minus4")?;
        }
        1 => {
            r.skip(1, "delta_pic_order_always_zero_flag")?;
            r.se("offset_for_non_ref_pic")?;
            r.se("offset_for_top_to_bottom_field")?;
            let n = r.ue_max(255, "num_ref_frames_in_pic_order_cnt_cycle")?;
            for _ in 0..n {
                r.se("offset_for_ref_frame")?;
            }
        }
        _ => {}
    }
    r.ue_max(16, "max_num_ref_frames")?;
    r.skip(1, "gaps_in_frame_num_value_allowed_flag")?;
    // Level 6.2 allows 139 264 macroblocks: sqrt(8 * 139264) = 1055 on the long side.
    let width_mbs = r.ue_max(1055, "pic_width_in_mbs_minus1")? + 1;
    let height_map_units = r.ue_max(1055, "pic_height_in_map_units_minus1")? + 1;
    let frame_mbs_only = r.flag("frame_mbs_only_flag")?;
    if !frame_mbs_only {
        r.skip(1, "mb_adaptive_frame_field_flag")?;
    }
    r.skip(1, "direct_8x8_inference_flag")?;
    if r.flag("frame_cropping_flag")? {
        for _ in 0..4 {
            r.ue("frame_crop_offset")?;
        }
    }
    // A damaged VUI loses only the VUI, not the fields before it.
    let vui = if r.flag("vui_parameters_present_flag")? {
        vui(&mut r).ok()
    } else {
        None
    };
    Ok(Sps {
        profile_idc,
        constraints,
        level_idc,
        sps_id,
        chroma_format_idc,
        bit_depth_luma,
        bit_depth_chroma,
        frame_mbs_only,
        width_mbs,
        height_map_units,
        pic_order_cnt_type,
        vui,
    })
}

/// MaxDpbMbs for a level (H.264 Table A-1). Level 1b is level_idc 9, or 11 with
/// constraint_set3 in the Baseline, Main and Extended profiles.
fn max_dpb_mbs(level_idc: u8, profile_idc: u8, constraints: u8) -> Option<u32> {
    let level_1b =
        level_idc == 11 && matches!(profile_idc, 66 | 77 | 88) && constraints & 0x10 != 0;
    Some(match level_idc {
        9 | 10 => 396,
        11 if level_1b => 396,
        11 => 900,
        12 | 13 | 20 => 2376,
        21 => 4752,
        22 | 30 => 8100,
        31 => 18000,
        32 => 20480,
        40 | 41 => 32768,
        42 => 34816,
        50 => 110_400,
        51 | 52 => 184_320,
        60..=62 => 696_320,
        _ => return None,
    })
}

impl Sps {
    /// Frames a decoder must hold before output may start, from the SPS alone: the
    /// VUI's `max_num_reorder_frames` when present; otherwise inferred as H.264 E.2.1
    /// does — 0 for the intra profiles (44, and 86/100/110/122/244 with
    /// constraint_set3) — plus 0 when `pic_order_cnt_type` is 2 (output order is decode
    /// order, §8.2.1.3); else the level's MaxDpbFrames (§A.3.1, capped at 16).
    pub fn reorder_depth(&self) -> u8 {
        self.reorder_depth_with(None)
    }

    /// As [`Sps::reorder_depth`], but when the VUI is silent and the container's sample
    /// table says how far presentation order departs from decode order (composition
    /// offsets, `SampleIndex::reorder_depth`), that measured depth is used instead of
    /// the level's worst case. Slices are never parsed to find it (SPEC §15.0).
    pub fn reorder_depth_with(&self, container: Option<u8>) -> u8 {
        if let Some(n) = self.vui.as_ref().and_then(|v| v.max_num_reorder_frames) {
            return u8::try_from(n).unwrap_or(16);
        }
        if let Some(d) = container {
            return d.min(16);
        }
        let intra =
            matches!(self.profile_idc, 86 | 100 | 110 | 122 | 244) && self.constraints & 0x10 != 0;
        if self.profile_idc == 44 || intra || self.pic_order_cnt_type == 2 {
            return 0;
        }
        let mbs = self.width_mbs * self.height_map_units * if self.frame_mbs_only { 1 } else { 2 };
        match max_dpb_mbs(self.level_idc, self.profile_idc, self.constraints) {
            Some(dpb) if mbs > 0 => u8::try_from((dpb / mbs).min(16)).unwrap_or(16),
            _ => 16,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn unescape_removes_emulation_prevention() {
        assert_eq!(
            unescape(&[0, 0, 3, 1, 0, 0, 3, 0, 0, 3]),
            vec![0, 0, 1, 0, 0, 0, 0]
        );
    }

    #[test]
    fn avcc_structure() {
        let avcc = [
            1, 0x64, 0, 0x28, 0xFF, 0xE1, 0, 2, 0x67, 0x64, 1, 0, 1, 0x68,
        ];
        let c = parse_avcc(&avcc).unwrap();
        assert_eq!((c.profile, c.level, c.nal_length_size), (0x64, 0x28, 4));
        assert_eq!(c.sps, vec![vec![0x67, 0x64]]);
        assert_eq!(c.pps_count, 1);
        for n in 0..avcc.len() - 1 {
            assert!(parse_avcc(&avcc[..n]).is_err() || n >= 10);
        }
    }

    #[test]
    fn truncated_sps_is_an_error() {
        let sps = [0x67, 0x64, 0x00, 0x28, 0xAC];
        assert!(parse_sps(&sps).is_err());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod known_answers {
    use super::*;
    use crate::codec::bits::BitWriter;

    fn nal(w: &BitWriter) -> Vec<u8> {
        let mut v = vec![0x67];
        v.extend(w.bytes());
        v
    }

    #[test]
    fn high_interlaced_scaling_poc1_full_vui() {
        let mut w = BitWriter::default();
        w.put(100, 8).put(0, 8).put(40, 8).ue(0); // profile, constraints, level, sps id
        w.ue(1).ue(0).ue(0).flag(false); // 4:2:0, 8-bit, 8-bit, bypass
        w.flag(true); // seq_scaling_matrix_present
        w.flag(true).se(-8); // list 0: delta -8 -> next 0, stop
        for _ in 1..8 {
            w.flag(false);
        }
        w.ue(0); // log2_max_frame_num_minus4
        w.ue(1).flag(false).se(-2).se(1).ue(2).se(3).se(-4); // POC type 1
        w.ue(4).flag(false); // max_num_ref_frames, gaps
        w.ue(119).ue(33).flag(false).flag(true); // 120 mbs wide, 34 map units, interlaced, mbaff
        w.flag(true); // direct_8x8
        w.flag(true).ue(0).ue(0).ue(0).ue(4); // cropping
        w.flag(true); // vui
        w.flag(true).put(255, 8).put(4, 16).put(3, 16); // SAR 4:3
        w.flag(true).flag(false); // overscan
        w.flag(true)
            .put(5, 3)
            .flag(true)
            .flag(true)
            .put(1, 8)
            .put(1, 8)
            .put(1, 8); // signal + colour
        w.flag(true).ue(1).ue(1); // chroma loc
        w.flag(true).put(1001, 32).put(60000, 32).flag(true); // timing
        w.flag(true)
            .ue(0)
            .put(4, 4)
            .put(6, 4)
            .ue(1000)
            .ue(2000)
            .flag(false)
            .put(23, 5)
            .put(23, 5)
            .put(23, 5)
            .put(24, 5);
        w.flag(false).flag(false).flag(false); // vcl hrd, low_delay, pic_struct
        w.flag(true)
            .flag(true)
            .ue(2)
            .ue(1)
            .ue(16)
            .ue(16)
            .ue(3)
            .ue(4); // bitstream restriction
        let s = parse_sps(&nal(&w)).unwrap();
        assert_eq!(
            (s.profile_idc, s.level_idc, s.chroma_format_idc),
            (100, 40, 1)
        );
        assert!(!s.frame_mbs_only);
        assert_eq!(
            (s.width_mbs, s.height_map_units, s.pic_order_cnt_type),
            (120, 34, 1)
        );
        let v = s.vui.clone().unwrap();
        assert_eq!(
            (v.video_full_range, v.colour),
            (Some(true), Some((1, 1, 1)))
        );
        assert_eq!(
            (v.max_num_reorder_frames, v.max_dec_frame_buffering),
            (Some(3), Some(4))
        );
        assert_eq!(s.reorder_depth(), 3);
    }

    #[test]
    fn high444_separate_planes_poc2_no_vui() {
        let mut w = BitWriter::default();
        w.put(244, 8).put(0, 8).put(51, 8).ue(3);
        w.ue(3).flag(true).ue(2).ue(2).flag(false); // 4:4:4, separate planes, 10-bit
        w.flag(true);
        for i in 0..12 {
            w.flag(i == 7);
            if i == 7 {
                for _ in 0..64 {
                    w.se(0); // flat list: next stays 8, all 64 deltas read
                }
            }
        }
        w.ue(0).ue(2); // POC type 2
        w.ue(1)
            .flag(false)
            .ue(239)
            .ue(134)
            .flag(true)
            .flag(true)
            .flag(false)
            .flag(false);
        let s = parse_sps(&nal(&w)).unwrap();
        assert_eq!(
            (
                s.chroma_format_idc,
                s.bit_depth_luma,
                s.bit_depth_chroma,
                s.sps_id
            ),
            (3, 10, 10, 3)
        );
        assert_eq!(
            s.reorder_depth(),
            0,
            "POC type 2: output order is decode order"
        );
    }

    #[test]
    fn level_1b_and_dpb_bound() {
        let mut w = BitWriter::default();
        w.put(66, 8).put(0x50, 8).put(11, 8).ue(0); // Baseline, set1+set3, level 11 = 1b
        w.ue(0)
            .ue(0)
            .ue(0)
            .ue(1)
            .flag(false)
            .ue(10)
            .ue(8)
            .flag(true)
            .flag(true)
            .flag(false)
            .flag(false);
        let s = parse_sps(&nal(&w)).unwrap();
        assert_eq!(s.reorder_depth(), 4, "396 / (11 * 9) = 4");
        assert_eq!(s.reorder_depth_with(Some(0)), 0);
    }

    #[test]
    fn damaged_vui_keeps_the_sps() {
        let mut w = BitWriter::default();
        w.put(77, 8)
            .put(0, 8)
            .put(30, 8)
            .ue(0)
            .ue(0)
            .ue(0)
            .ue(0)
            .ue(1)
            .flag(false)
            .ue(44)
            .ue(35);
        w.flag(true).flag(true).flag(false).flag(true); // vui present ...
        w.flag(true); // aspect_ratio_info_present then nothing: truncated
        let s = parse_sps(&nal(&w)).unwrap();
        assert!(s.vui.is_none());
        assert_eq!(s.level_idc, 30);
    }
}
