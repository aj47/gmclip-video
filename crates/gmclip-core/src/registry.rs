//! The command registry: one explicit list, no linker tricks (SPEC §3.2).
//! `cargo xt check-registry` fails CI if a `#[command]` type is not listed here, a
//! listed type has no `#[command]`, a name is invalid or repeated, or the list is not
//! sorted by command name; the tests below check the same at runtime.

use std::fmt;
use std::marker::PhantomData;

use gmclip_proto::CmdError;

use crate::command::{Command, CommandMeta, Cx};

/// A command behind a type-erased, JSON-in/JSON-out call.
pub trait ErasedCommand: Sync {
    fn meta(&self) -> &'static CommandMeta;
    fn call(&self, args: serde_json::Value, cx: &mut Cx<'_>)
    -> Result<serde_json::Value, CmdError>;
    /// Parses `args` against the command's argument type without running it.
    fn check_args(&self, args: &serde_json::Value) -> Result<(), CmdError>;
}

/// Registers a [`Command`] type; `registry!` builds these.
pub struct Registered<C>(PhantomData<fn() -> C>);

impl<C> Registered<C> {
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<C> Default for Registered<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: Command> fmt::Debug for Registered<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Registered({})", C::META.name)
    }
}

fn parse<C: Command>(args: serde_json::Value) -> Result<C::Args, CmdError> {
    // serde's messages can quote caller input; report the position only (R-CMD-6).
    serde_json::from_value(args).map_err(|e| CmdError::Invalid {
        path: "/args".to_owned(),
        reason: format!(
            "does not match the schema ({})",
            match e.classify() {
                serde_json::error::Category::Data => "wrong type, missing or unknown field",
                serde_json::error::Category::Syntax => "syntax",
                serde_json::error::Category::Eof => "truncated",
                serde_json::error::Category::Io => "io",
            }
        ),
    })
}

impl<C: Command> ErasedCommand for Registered<C> {
    fn meta(&self) -> &'static CommandMeta {
        const { &C::META }
    }

    fn call(
        &self,
        args: serde_json::Value,
        cx: &mut Cx<'_>,
    ) -> Result<serde_json::Value, CmdError> {
        let args = parse::<C>(args)?;
        let out = C::run(args, cx)?;
        serde_json::to_value(out).map_err(|_| CmdError::Internal {
            trace_id: "serialise-output".to_owned(),
        })
    }

    fn check_args(&self, args: &serde_json::Value) -> Result<(), CmdError> {
        parse::<C>(args.clone()).map(|_| ())
    }
}

/// Builds the static registry from a list of command types (sorted by command name).
#[macro_export]
macro_rules! registry {
    ($($cmd:ty),* $(,)?) => {
        /// Every registered command, sorted by name.
        pub static REGISTRY: &[&dyn $crate::registry::ErasedCommand] =
            &[$(&$crate::registry::Registered::<$cmd>::new()),*];
    };
}

/// The command registered under `name`.
pub fn find(name: &str) -> Option<&'static dyn ErasedCommand> {
    REGISTRY
        .binary_search_by(|c| c.meta().name.cmp(name))
        .ok()
        .map(|i| REGISTRY[i])
}

// The list `cargo xt check-registry` reads. Sorted by command name.
registry! {
    crate::commands::analysis::AsrActivate,
    crate::commands::analysis::AsrRun,
    crate::commands::overlay::AssetAdd,
    crate::commands::sound::AudioOverlayAdd,
    crate::commands::sound::AudioOverlayDelete,
    crate::commands::sound::AudioOverlaySet,
    crate::commands::brand::BrandKitSet,
    crate::commands::caption::CaptionCueBreak,
    crate::commands::caption::CaptionEdit,
    crate::commands::caption::CaptionKeepSet,
    crate::commands::caption::CaptionKeywordToggle,
    crate::commands::polish::CaptionKeywordsMark,
    crate::commands::caption::CaptionKeywordsSet,
    crate::commands::library::CaptionPresetApply,
    crate::commands::library::CaptionPresetList,
    crate::commands::library::CaptionPresetSave,
    crate::commands::vocab::CaptionSet,
    crate::commands::library::CaptionsExport,
    crate::commands::project::ChatSpikesCmd,
    crate::commands::comment::CommentAdd,
    crate::commands::comment::CommentDelete,
    crate::commands::comment::CommentResolve,
    crate::commands::vocab::ContentHoldAdd,
    crate::commands::vocab::ContentHoldDelete,
    crate::commands::packaging::CopyDraftState,
    crate::commands::packaging::CopyDraftsAdd,
    crate::commands::packaging::CopyDraftsList,
    crate::commands::matte::CropFollowAction,
    crate::commands::matte::CropFollowApply,
    crate::commands::cursor::CropFollowCursor,
    crate::commands::look::CropSet,
    crate::commands::cursor::CropZoomFocus,
    crate::commands::cursor::CursorFollowApply,
    crate::commands::cut::CutCreate,
    crate::commands::compose::CutEdit,
    crate::commands::polish::CutLoopSet,
    crate::commands::longform::CutParentDiff,
    crate::commands::longform::CutPullFromParent,
    crate::commands::edit::CutSet,
    crate::commands::comment::EditProposalState,
    crate::commands::comment::EditSuggest,
    crate::commands::brand::EndCardAdd,
    crate::commands::brand::EndCardLongformAdd,
    crate::commands::brand::EndCardLongformFit,
    crate::commands::topic::FluffPropose,
    crate::commands::topic::FluffState,
    crate::commands::text::FontAdd,
    crate::commands::longform::FramingAdd,
    crate::commands::cut::FramingCreate,
    crate::commands::vocab::FramingSet,
    crate::commands::vocab::GainSegmentAdd,
    crate::commands::vocab::GainSegmentDelete,
    crate::commands::vocab::GainSegmentSet,
    crate::commands::project::Help,
    crate::commands::vocab::HideRolesAdd,
    crate::commands::vocab::HideRolesDelete,
    crate::commands::history::HistoryDiffCmd,
    crate::commands::history::HistoryListCmd,
    crate::commands::hold::HoldScreenHere,
    crate::commands::packaging::HookTextSuggest,
    crate::commands::project::JobCancel,
    crate::commands::project::JobWait,
    crate::commands::look::LayerAdd,
    crate::commands::look::LayerDelete,
    crate::commands::look::LayerFrameSet,
    crate::commands::edit::LayerSet,
    crate::commands::overlay::LayerTrackSet,
    crate::commands::screenlook::LayoutBuiltinAdd,
    crate::commands::screenlook::LayoutBuiltinList,
    crate::commands::look::LayoutCreate,
    crate::commands::compose::LayoutEdit,
    crate::commands::look::LayoutSet,
    crate::commands::project::LibrarySearch,
    crate::commands::longform::LongformChapters,
    crate::commands::longform::LongformCompare,
    crate::commands::longform::LongformFork,
    crate::commands::longform::LongformFromTopics,
    crate::commands::longform::LongformPick,
    crate::commands::polish::LoopFix,
    crate::commands::brand::LowerThirdAdd,
    crate::commands::marker::MarkerList,
    crate::commands::matte::MatteBrush,
    crate::commands::matte::MatteQuality,
    crate::commands::matte::MatteSet,
    crate::commands::analysis::MeasurementAdd,
    crate::commands::moment::MomentAdd,
    crate::commands::moment::MomentExcerptSet,
    crate::commands::moment::MomentMark,
    crate::commands::moment::MomentSetState,
    crate::commands::moment::MomentsFoundAdd,
    crate::commands::moment::MomentsList,
    crate::commands::opening::OpeningAdd,
    crate::commands::opening::OpeningChoose,
    crate::commands::opening::OpeningDelete,
    crate::commands::opening::OpeningSet,
    crate::commands::opening::OpeningsList,
    crate::commands::overlay::OverlayAdd,
    crate::commands::edit::OverlayDelete,
    crate::commands::overlay::OverlayMove,
    crate::commands::edit::OverlaySet,
    crate::commands::overlay::OverlaySplit,
    crate::commands::overlay::OverlaySplitParts,
    crate::commands::overlay::OverlayStackSet,
    crate::commands::brand::OverlayTemplateInsert,
    crate::commands::brand::OverlayTemplateSave,
    crate::commands::overlay::OverlayTrackSet,
    crate::commands::part::PartAdd,
    crate::commands::vocab::PartCaptionPlace,
    crate::commands::edit::PartDelete,
    crate::commands::part::PartMerge,
    crate::commands::part::PartReorder,
    crate::commands::part::PartSetGain,
    crate::commands::part::PartSetRamp,
    crate::commands::edit::PartSetRole,
    crate::commands::part::PartSetSpeed,
    crate::commands::sound::PartSetUnder,
    crate::commands::part::PartSplit,
    crate::commands::part::PartTrim,
    crate::commands::part::PartsGroup,
    crate::commands::part::PartsUngroup,
    crate::commands::look::PlaceSet,
    crate::commands::project::ProjectList,
    crate::commands::longform::ProjectMapCmd,
    crate::commands::project::ProjectOpen,
    crate::commands::polish::PunchInsSetCmd,
    crate::commands::history::Redo,
    crate::commands::history::RestorePointCreate,
    crate::commands::history::RestoreTo,
    crate::commands::review::ReviewAdd,
    crate::commands::history::RunBegin,
    crate::commands::handoff::RunChoiceRestore,
    crate::commands::overlay::RunClockMake,
    crate::commands::history::RunEnd,
    crate::commands::handoff::RunSummarySet,
    crate::commands::history::RunUndo,
    crate::commands::section::SectionAdd,
    crate::commands::section::SectionAutoApply,
    crate::commands::section::SectionList,
    crate::commands::section::SectionRemove,
    crate::commands::section::SectionSet,
    crate::commands::cut::SourceAdd,
    crate::commands::analysis::SourceAnalyze,
    crate::commands::cut::SourceAudioRolesSet,
    crate::commands::source::SourceCursorSet,
    crate::commands::source::SourceImport,
    crate::commands::source::SourceList,
    crate::commands::marker::SourceMarkersSet,
    crate::commands::source::SourceRelink,
    crate::commands::source::SourceRemove,
    crate::commands::source::SourceStreamLayoutSet,
    crate::commands::story::StoryApply,
    crate::commands::story::StoryBriefSet,
    crate::commands::story::StorySuggestionState,
    crate::commands::story::StorySuggestionsSet,
    crate::commands::topic::StreamEditEnsure,
    crate::commands::analysis::StreamProcess,
    crate::commands::stylelib::StyleLibraryApply,
    crate::commands::stylelib::StyleLibraryList,
    crate::commands::stylelib::StyleLibrarySave,
    crate::commands::analysis::SweepApply,
    crate::commands::analysis::SweepFillers,
    crate::commands::analysis::SweepPauses,
    crate::commands::analysis::SweepPreviewCmd,
    crate::commands::analysis::SweepRetakes,
    crate::commands::cut::SyncGroupCreate,
    crate::commands::cut::SyncOffsetSet,
    crate::commands::sound::SyncUnderSet,
    crate::commands::caption::TermFix,
    crate::commands::caption::TermSet,
    crate::commands::text::TextStyleSet,
    crate::commands::comment::ThumbnailAdd,
    crate::commands::comment::ThumbnailSet,
    crate::commands::topic::TopicDelete,
    crate::commands::topic::TopicMerge,
    crate::commands::topic::TopicSet,
    crate::commands::topic::TopicSplit,
    crate::commands::topic::TopicState,
    crate::commands::history::Undo,
    crate::commands::look::VisualSplitAdd,
    crate::commands::vocab::VisualSplitDelete,
    crate::commands::vocab::VisualSplitSet,
    crate::commands::caption::WordEdgeNudge,
    crate::commands::words::WordsFind,
    crate::commands::words::WordsReplace,
    crate::commands::words::WordsRestore,
    crate::commands::words::WordsStrike,
    crate::commands::section::ZoomAuto,
}

/// Commands only a human runs, refused to every agent identity whatever its scopes (the
/// feedback log, R-PROF-2; the Clippy hand-off, R-CLIP-1; the injection targets of R-INJ-4
/// cases 4 and 5). Matched loosely (case, `-`/`.`/space as `_`) so near-miss spellings are
/// refused too, even before such a command exists in the registry.
pub fn human_only(command: &str) -> bool {
    let c = command
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '.', ' '], "_");
    c == "feedback_append"
        || c.contains("clippy")
        || c.contains("handoff")
        || c.contains("send_to_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(n: &str) -> bool {
        (1..=64).contains(&n.len())
            && n.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }

    #[test]
    fn registry_is_sorted_and_names_are_valid() {
        let names: Vec<_> = REGISTRY.iter().map(|c| c.meta().name).collect();
        assert!(names.windows(2).all(|w| w[0] < w[1]), "{names:?}");
        assert!(names.iter().all(|n| valid(n)), "{names:?}");
        for n in &names {
            assert_eq!(find(n).map(|c| c.meta().name), Some(*n));
        }
        assert!(find("nope").is_none());
    }

    /// R-CMD-1: every command has docs, ≥ 1 example that parses, and schemas.
    #[test]
    fn every_command_has_metadata_and_valid_examples() {
        for c in REGISTRY {
            let m = c.meta();
            assert!(!m.doc.trim().is_empty(), "{}", m.name);
            assert!(!m.examples.is_empty(), "{}", m.name);
            for ex in m.examples {
                let v: serde_json::Value = serde_json::from_str(ex)
                    .unwrap_or_else(|e| panic!("{}: example is not JSON: {e}", m.name));
                c.check_args(&v)
                    .unwrap_or_else(|e| panic!("{}: example does not parse: {e} ({ex})", m.name));
            }
            let a = serde_json::to_value((m.args_schema)()).unwrap();
            let o = serde_json::to_value((m.output_schema)()).unwrap();
            assert!(a.is_object() && o.is_object(), "{}", m.name);
        }
    }
}
