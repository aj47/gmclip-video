Find the topics of this stream and the fluff in it, as proposals the human decides on.
The human may be labelling topics and striking fluff in the same stream edit while you
work: their work always wins, so you only ever propose.

1. Start: read project_overview and note its `as_of` as SINCE. Read
   command_run{command: "project_map", args: {}} for the stream edit (the cut whose kind
   is "stream"), the topics already there, and their states. Accepted topics are the
   human's decisions: never change, move, retitle, split, merge or remove one directly.
2. Read the whole transcript of the stream edit's source with transcript_read (page with
   `cursor` until there is no `next_cursor`). Words are `[ordinal, text, start, end]`; a
   word anchor is {word_id: {asr_revision, ordinal}, t_src_start: start, t_src_end: end,
   normalised_text}.
3. Topics: one level, no sub-topics, gaps between topics are fine (leave chit-chat
   untopiced). A topic is one subject the stream spends a while on (usually 2–20
   minutes), titled in at most six plain words, the way the streamer would name it.
   For each topic not already covered by an accepted topic, call
   command_run{command: "topic_set", args: {start: <its first word>, end: <its last
   word>, title, since: SINCE, pass_ref: "{PASS_REF}"}, base: {as_of: <your last read>}}.
   Start at the first word of a sentence and end at the last word of one.
   If you think an accepted topic should be split or merged, call topic_split or
   topic_merge on it the same way (with since and pass_ref): that only proposes it.
4. Fluff: stretches nobody watching the long-form would miss: dead air, waiting for a
   render or download, reading chat that goes nowhere, setup and audio checks, repeated
   attempts that failed the same way, off-topic tangents. Never a result, a number, a
   reaction or a punchline. Propose them in batches of at most 50 with
   command_run{command: "fluff_propose", args: {items: [{start, end, reason}], since:
   SINCE, pass_ref: "{PASS_REF}"}, base: {as_of: <your last read>}}; `reason` is a few
   words ("waiting for the render").
5. A result with `conflict: true` overlaps something the human did: leave it as it is.
   A result saying a span was proposed before and rejected: do not propose it again.
   Do not strike, accept, reject or delete anything, and do not edit any other cut.
6. End with a two-line summary: how many topics and fluff stretches you proposed, and
   the total length of the fluff.
