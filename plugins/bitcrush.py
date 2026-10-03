#!/usr/bin/env python3
"""Bitcrush: example soundpipe exec plugin (python3 stdlib only).

Protocol: raw interleaved f32le on stdin -> same number of samples on stdout,
flushed after every chunk. Settings come from environment variables.
"""
import array
import os
import sys

channels = int(os.environ.get("SP_CHANNELS", "2"))
bits = int(float(os.environ.get("SP_PARAM_BITS", "8")))
hold = max(1, int(float(os.environ.get("SP_PARAM_DOWNSAMPLE", "4"))))
levels = 2.0 ** (bits - 1)

stdin = sys.stdin.buffer
stdout = sys.stdout.buffer
frame_bytes = 4 * channels
held = [0.0] * channels
counter = 0
carry = b""

while True:
    data = stdin.read1(65536)
    if not data:
        break
    data = carry + data
    usable = len(data) // frame_bytes * frame_bytes
    carry = data[usable:]
    samples = array.array("f")
    samples.frombytes(data[:usable])
    if sys.byteorder == "big":
        samples.byteswap()
    for i in range(0, len(samples), channels):
        if counter == 0:
            for c in range(channels):
                x = samples[i + c]
                if x != x or x in (float("inf"), float("-inf")):
                    x = 0.0
                x = max(-1.0, min(1.0, x))
                held[c] = round(x * levels) / levels
        counter = (counter + 1) % hold
        for c in range(channels):
            samples[i + c] = held[c]
    if sys.byteorder == "big":
        samples.byteswap()
    stdout.write(samples.tobytes())
    stdout.flush()
