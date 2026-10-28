# Herbert notification cues

These original sounds use additive synthesis from scripts/generate_sounds.py.
They contain no recordings, third-party samples or Windows alert sounds.
The repository license applies.

- batch.wav: one soft, descending motor glide, 0.26 seconds.
- complete.wav: two soft glides, 0.44 seconds.
- attention.wav: two lower glides, 0.44 seconds.

The shipped files are mono, 24 kHz, signed 16-bit PCM WAV. Smooth envelopes
avoid clicks. Peak levels are below 0.10 of full scale. This is an authored
volume choice, not a limit on replacement files.

This sounds folder belongs beside herbert.exe. Replace any WAV using the same
filename. Herbert reads it at the next cue and closes the file before playback.
No restart is needed. A missing or unreadable file is silent. There is no
Windows alert fallback. Preserve your replacements when updating the app.

Only the GUI plays these files. The saved Sounds checkbox controls all cues.
CLI and MCP do not start an audio worker.
