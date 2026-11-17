# Talking head / podcast

Use this kit for one or more people talking to camera or to each other. The frame is
source-true or a centre crop on the speaker. Captions carry the story. Moments are
complete thoughts with a point. Captions use the "Readable" preset. The rubric your agent
scores with is `talking-head.rubric.md`.

```json
{
  "rubric": "Score a moment by whether it is a complete thought with a point: a claim and its reason, a story and its turn, or a question and its answer.",
  "duration_bands": {
    "9:16": [20, 90],
    "16:9": [60, 900]
  },
  "default_template": "talking_head",
  "caption_grouping": "readable_phrase",
  "text_hook": true,
  "loudness": {
    "mode": "static",
    "integrated_lufs": -14.0,
    "true_peak_dbtp": -1.0
  },
  "caption_preset": "readable",
  "default_framings": ["9:16"]
}
```
