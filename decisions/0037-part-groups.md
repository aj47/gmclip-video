# 0037 — Part groups and multi-select on the timeline

Date: 2026-10-18 · Decided by: AJ ("i should be able to group clips that ive broken up in the timeline so i can move/arrange them together"; both multi-select and sticky groups; parts with others between them land together) · Status: adopted

## Context
After splitting a take into several parts, arranging them meant dragging one part at a time. AJ asked to group parts so they move as one, and chose both a throwaway multi-selection and a group that stays.

## Decision
- **Model (additive, epoch 5)**: `Part.part_group?: Id<PartGroup>`, an id the members share (not an entity). A split keeps both halves in the group; older rows and ops keep their exact bytes.
- **Commands**: `parts_group {cut, parts[]}` (two or more; a part already grouped brings its whole group into the new one) and `parts_ungroup {cut, parts[]}` (each named part's whole group), one undo step each. The app's `parts_move_to {cut, parts[], index}` moves several parts as one `cut_edit` of `part_reorder` ops.
- **Timeline (§5.3)**: ⌘-click a part adds it to the selection, ⇧-click selects the run of parts up to it (elsewhere they keep marking and ranges); a click on a grouped part selects its group; dragging any selected part moves the selection (with whole groups) as one block with live slide. The timeline has no gaps, so parts with others between them land **back to back** at the drop, in their order; the parts that were between them stay where they were. ⌘G groups, ⌘⇧G ungroups (also the timeline's Group / Ungroup buttons and ⌘K). A group shows as a bar in its own colour over its parts.

## Evidence
`crates/gmclip-core/tests/part_groups.rs` (group, a group joining another, split keeps the group, ungroup by any member, undo); `app/ui/tests/e2e/partgroups.spec.ts`.

## Sections changed
- `spec/SPEC.md` §4.3 `Part` (`part_group?`), §5.3 (multi-select, groups)
