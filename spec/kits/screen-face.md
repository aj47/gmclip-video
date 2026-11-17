# Screen + face

Use this kit for coding, tutorials and builds shown on screen with a face camera. The
layout is a screen panel at the top and the face below. Activity crop follows what
changes on screen. At setup the kit asks for words the transcriber gets wrong, such as
product names, and saves them as terms. A text hook states the result. Captions use the
"Readable" preset. The rubric your agent scores with is `screen-face.rubric.md`.

```json
{
  "rubric": "Score a moment by whether it shows a result a viewer can see: something built, fixed, measured or broken on screen. The claim must be visible, not only spoken.",
  "duration_bands": {
    "9:16": [20, 75],
    "16:9": [180, 900]
  },
  "default_template": "screen_face",
  "activity_crop": true,
  "terms_prompt": true,
  "text_hook": true,
  "caption_grouping": "readable_phrase",
  "loudness": {
    "mode": "static",
    "integrated_lufs": -14.0,
    "true_peak_dbtp": -1.0
  },
  "caption_preset": "readable",
  "default_framings": ["9:16", "16:9"]
}
```
