"""Generate Herbert's original, replaceable PCM notification cues."""
from pathlib import Path
import math
import struct
import wave

ROOT = Path(__file__).resolve().parents[1] / 'assets/sounds'
RATE = 24000


def write(name, duration, notes):
    samples = []
    for index in range(round(duration * RATE)):
        time = index / RATE
        sample = 0.0
        for start, frequency, gain, length in notes:
            age = time - start
            if age < 0 or age >= length:
                continue
            # A rounded motor glide suggests a small vacuum winding down.
            # Smooth edges and few harmonics avoid clicks and a nasal horn.
            envelope = math.sin(math.pi * age / length) ** 2
            phase = math.tau * frequency * (age - 0.22 * age * age / length)
            tone = 0.88 * math.sin(phase) + 0.12 * math.sin(phase * 2)
            sample += gain * envelope * tone
        sample *= min(1.0, (duration - time) / 0.025)
        assert abs(sample) < 1.0, 'The authored cue must not clip.'
        samples.append(round(sample * 32767))
    ROOT.mkdir(parents=True, exist_ok=True)
    with wave.open(str(ROOT / name), 'wb') as output:
        output.setnchannels(1)
        output.setsampwidth(2)
        output.setframerate(RATE)
        output.writeframes(struct.pack('<' + 'h' * len(samples), *samples))
    print(f'{name}: {duration:.2f}s, peak {max(abs(n) for n in samples) / 32767:.3f}')


if __name__ == '__main__':
    write('batch.wav', 0.26, [(0.0, 340, 0.10, 0.24)])
    write('complete.wav', 0.44, [(0.0, 340, 0.10, 0.18), (0.21, 430, 0.09, 0.20)])
    write('attention.wav', 0.44, [(0.0, 310, 0.09, 0.18), (0.21, 260, 0.09, 0.20)])
