//! Caption cue builders (SPEC §5.10). M0: [`story_v5`], the exact port of the reference
//! story captions (`caption_cues` + `write_ass`), and [`pillow`], their fit measure.

pub mod pillow;
pub mod story_v5;

use std::path::PathBuf;

/// Where the story captions' face lives on AJ's machine; `GMCLIP_CAPTION_FONT` overrides
/// it. Commercial, never committed: the reference builds work only where it is installed.
pub fn caption_font_path() -> PathBuf {
    std::env::var_os("GMCLIP_CAPTION_FONT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if cfg!(windows) {
                // a per-user install there
                PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts")
                    .join("Neometric Heavy (Bold).otf")
            } else {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join("Library/Fonts/Neometric Heavy (Bold).otf")
            }
        })
}
