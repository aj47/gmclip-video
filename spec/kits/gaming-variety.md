# Gaming / variety

Use this kit for gameplay with a facecam: reactions, clutches, fails and chat moments.
Clips are short and land on the moment. The layout splits the screen: the camera fills
the top 30% of the frame, the game sits below, and captions go in the seam between them.
The camera is not keyed. Captions use adaptive word grouping (the "Bold pop" preset), and
there is no text hook by default. Trigger phrases are hints in your agent's find-moments
brief, never moments by themselves (gmclip finds no moments without your agent). The
rubric your agent scores with is `gaming-variety.rubric.md`.

```json
{
  "rubric": "Score a moment by how fast a stranger gets it: a reaction, a clutch, a fail, or a chat moment that lands without the stream's context. Prefer the beat right before the payoff and the reaction right after it.",
  "duration_bands": {
    "9:16": [8, 45],
    "16:9": [8, 120]
  },
  "default_template": "gameplay_facecam_split",
  "caption_grouping": "adaptive_words",
  "text_hook": false,
  "trigger_phrases": ["clip that", "let's go", "no way", "did you see that", "oh my god", "are you kidding"],
  "loudness": {
    "mode": "static",
    "integrated_lufs": -14.0,
    "true_peak_dbtp": -1.0
  },
  "caption_preset": "bold_pop",
  "default_framings": ["9:16"]
}
```
