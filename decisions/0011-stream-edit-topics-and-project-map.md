# 0011 — Stream edit, topics, and the project map

Status: accepted (AJ, 2026-10-15: "go with recommendation but dont need to wait")

## AJ's workflow, in his words

> stream -> ai identifies topics whilst i also am manually going through, cutting and labelling topics (we need to be able to select / highlight large parts of text and easily label/segment the topic) as well as select parts and remove fluff -> then i will have a longform(s) as well as clip options for individual highlights that is a bit more organized. i might have multiple different vertical crops for the same starter stream and one or multiple long form clips but i should somehow be organized enough to jump between stream/longform/shortform all in one project

## Decision

1. **The stream edit.** Each project gets one *stream edit*: a Cut with `kind: stream` that covers the whole source (or sync group) in source order. Striking text in it removes fluff for everything made from the stream. It is a normal cut in every other respect: it has words, history, undo, and a 16:9 framing, and it can be exported as-is.
2. **Topics** are a new entity on the stream edit. A topic is a labelled, word-edged range of the stream. Topics don't overlap at one level, and gaps between them are allowed (untopiced talk). The fields are `{id, cut (the stream edit), span{start: Anchor, end: Anchor}, title, colour, origin: human|agent, state: proposed|accepted|rejected, notes, order}`.
   - A human's topics are decisions. An agent proposes topics only as `proposed` (drawn dashed) and never moves or retitles an accepted topic, which is the same rule as R-STORY-1.
   - An agent may propose a split or a merge, and that change happens only when the human applies it.
3. **Large-range text selection** in the stream edit's transcript works like this:
   - **Selecting:** drag, ⇧-click to extend across minutes, and ⌥-click to select a whole paragraph or line.
   - **Topics:** `t` labels the selection as a topic. A popover offers recent titles, or type a new one, and Enter applies it. `⇧t` splits the topic at the caret.
   - **Topic bands:** topic edges can be dragged in the text margin band or on the timeline topic lane.
   - **Fluff:** `d` / `x` strike the selection, exactly as the words strip does now.
   - **Big strikes:** a strike over 30 s shows a collapsed "– 2:14 struck" row that expands on click, so the text stays readable.
4. **AI works alongside the human.** The topic pass and fluff pass run through the user's own agent CLI (the "any agent I have, on its own subscription" pick). Proposals stream in live as dashed topic bands and as sweep-style fluff proposals, each with ▶ preview, accept and reject. A human edit made while a pass runs always wins: a proposal that overlaps human work arrives already marked "conflicts with your edit" and is never applied silently.
5. **Long-forms are derived from the stream edit.**
   - **Creating one:** "Make long-form from topics…" (tick topics, reorder them) creates a 16:9 cut. Its parts are the kept stretches of those topics, and it records `lineage{forked_from: stream edit, topics[]}`. Several long-forms per project are allowed.
   - **Later changes to the stream edit** (more fluff struck, a topic edge moved) show on each long-form as "Stream edit changed: N edits", with a diff and **Pull changes**. Nothing propagates silently, because a long-form's own trims are decisions too.
   - **Chapters:** each topic becomes a chapter, so YouTube chapters come free from the topics (a round-one pick).
6. **Shorts sit under topics.** Moments group under the topic that contains them, and "make a short" from a moment works as now. A short can have **several 9:16 framings** (for example "screen focus" and "face big"), each with its own crops and layouts, and they share one set of words and captions. These are separate Framing entities on one cut; making separate cuts would split the words and history.
7. **Project map.** One navigator, always on the left at desktop size, and a sheet at tablet size:
   ```
   ▾ Stream edit            2:58:10 → 2:31:44 kept
       ▸ Topics (14)        ● Setup  ● GPU prices  ○ Tangent (proposed) …
   ▾ Long-forms (2)
       Local AI video, part 1   16:9 · 24:10 · topics 2,3,5
       GPU deep dive           16:9 · 11:02 · topic 3
   ▾ Shorts (by topic)
       GPU prices
         "My PC made this in 67 s"   9:16 ×2 (screen focus, face big) · 0:58
   ```
   - **Jumping between levels:** "Show in stream" (⌘↑) and "Show in long-form" go from the selected word in a short to the same word in the parent. "Shorts here" (⌘↓) goes from a stream or long-form selection to the shorts that use it. Every time is a source anchor, so these mappings are exact.
   - **Badges:** each item shows framings, duration, export state (fresh, stale, never) and review state.
   - **Coverage chips:** the stream edit's transcript shows which ranges are used by which long-form or short, so overlap between siblings is visible (and lint warns about it, §5.13).

## Why

- AJ edits long-form and shorts from the same stream, and today they live in separate places with no link between them.
- Topics give the stream a structure that both humans and agents can work in, and chapters, moment grouping and library search (another pick) all hang off them.
- Building on the existing Cut, Framing, word-strike and sweep machinery keeps this to one new entity (Topic) plus `Cut.kind`, rather than a parallel editor.

## Cost and gates

- **Model:** `Topic`, `Cut.kind: stream|long|short`, and `lineage.topics[]`. Commands: `topic_set` / `topic_split` / `topic_merge` / `topic_state`, `longform_from_topics`, and `cut_pull_from_parent`. These are reachable through MCP like every other command.
- **Performance:** R-WORD-1 [M2] already covers strike and restore on a 3-hour transcript. Topic labelling over a 3-hour stream must meet the same ≤ 16 ms core time.
- **Placement:** now, in parallel with the M2 work (AJ: "dont need to wait").

## AJ's answers

1. Gaps between topics are allowed.
2. A long-form always asks before pulling stream-edit changes.
3. One level of topics; no sub-topics for now.
4. Build it now, without waiting for the M2 shorts gate.
