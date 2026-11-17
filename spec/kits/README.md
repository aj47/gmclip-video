# Kits (SPEC §10.5)

A kit is a starting profile for a kind of recording. Each kit has these files:

- `<kit>.md`: what the kit is for, and its profile content as one fenced `json` block.
  `gmclip profile new --kit <kit>` copies that block into a new profile's
  `profile.json` under `content`.
- `<kit>.style.md`: the kit's starting style rules. It is copied into the new profile as
  `style.md`. It only takes effect when the Owner points the style setting at it
  (`gmclip owner style set <profile>/style.md`).

- `<kit>.rubric.md` (not for Blank): the moment-scoring rubric as a versioned prompt
  asset (R-AI-5). A profile made from the kit carries its text as `rubric` and its
  reference `<kit>@<version>:<sha256 prefix>` as `rubric_ref`; the find-moments and
  grow-story briefs your agent gets append it (as gmclip's own text while it is unchanged,
  as untrusted profile text once edited), with the kit's duration band and, for gaming,
  its trigger phrases as hints.

What each kit sets when a cut gets a new 9:16 framing (SPEC §5.7.1 step 2, "template
wins"): `gameplay_facecam_split` puts the camera across the top 30% (cover-cropped from
the source's camera box), the game below and the captions in the seam, no key;
`screen_face` is the screen panel with the face below (AJ's steady layout);
`talking_head` (and Blank) a centred crop. A template that needs a camera box falls back
to the centred crop when the source has none. The kit's `caption_preset` styles the new
framing's captions and its static loudness is the framing's export loudness.

A kit holds content only. It never sets a harness, a budget, scopes, roots or a style
path (R-PROF-4). AJ's own rules live only in AJ's profile, not in any kit.
