#!/usr/bin/env python3
"""Dex local music & SFX generation sidecar.

Managed by Dex-Harness (`harness-audio`). It is written to
`~/.dex/runtimes/audiogen/dex_audiogen_server.py` and runs inside the managed
`~/.dex/runtimes/mlx-venv` interpreter. Only the Python standard library plus the
packages installed by the `audiogen` runtime (torch, transformers, soundfile,
optionally diffusers) are used.

Endpoints (all bound to 127.0.0.1):
  GET  /health            -> JSON status (always fast; models load lazily)
  POST /v1/audio/music    -> audio/wav  {"prompt", "lyrics"?, "duration_secs"?, "seed"?}
  POST /v1/audio/sfx      -> audio/wav  {"prompt", "duration_secs"?, "seed"?}

Backends:
  music: Meta MusicGen through Hugging Face transformers (MPS on Apple Silicon).
         Model id from DEX_MUSICGEN_MODEL (default facebook/musicgen-small).
  sfx:   MusicGen conditioned on the effect description (default). Set
         DEX_SFX_BACKEND=audioldm to use diffusers AudioLDM (DEX_SFX_MODEL, default
         cvssp/audioldm-s-full-v2); note diffusers' AudioLDM pipeline is deprecated
         and yields near-silent audio with transformers 5.x, so silent output falls
         back to MusicGen. The backend used is reported in the X-Dex-Backend header.

Every response is checked for silence; a silent render is never returned as success.
"""

from __future__ import annotations

import argparse
import io
import json
import os
import sys
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

VERSION = "1"
MUSICGEN_MODEL = os.environ.get("DEX_MUSICGEN_MODEL", "facebook/musicgen-small")
SFX_MODEL = os.environ.get("DEX_SFX_MODEL", "cvssp/audioldm-s-full-v2")
SFX_BACKEND = os.environ.get("DEX_SFX_BACKEND", "musicgen").strip().lower()
SILENCE_PEAK = 1e-3
MAX_MUSIC_SECS = 120.0
MAX_SFX_SECS = 30.0

_lock = threading.Lock()
_musicgen = None  # (processor, model, device)
_audioldm = None  # (pipeline, device)


def log(msg: str) -> None:
    print(f"[dex-audiogen] {msg}", file=sys.stderr, flush=True)


def _device():
    import torch

    if torch.backends.mps.is_available():
        return "mps"
    if torch.cuda.is_available():
        return "cuda"
    return "cpu"


def _use_audioldm() -> bool:
    return SFX_BACKEND == "audioldm" and _diffusers_available()


class SilentOutput(RuntimeError):
    pass


def _diffusers_available() -> bool:
    try:
        import importlib.util

        return importlib.util.find_spec("diffusers") is not None
    except Exception:  # noqa: BLE001
        return False


def _load_musicgen():
    global _musicgen
    if _musicgen is None:
        from transformers import AutoProcessor, MusicgenForConditionalGeneration

        dev = _device()
        log(f"loading MusicGen '{MUSICGEN_MODEL}' on {dev} (first run downloads weights)")
        t0 = time.time()
        processor = AutoProcessor.from_pretrained(MUSICGEN_MODEL)
        import torch

        # fp16 halves memory on Apple Silicon / CUDA (stereo-large: ~8 GB instead of ~17 GB)
        # with the same output quality; DEX_MUSICGEN_DTYPE=fp32 forces full precision.
        want_fp32 = os.environ.get("DEX_MUSICGEN_DTYPE", "").strip().lower() == "fp32"
        dtype = torch.float32 if (dev == "cpu" or want_fp32) else torch.float16
        model = MusicgenForConditionalGeneration.from_pretrained(MUSICGEN_MODEL, torch_dtype=dtype).to(dev)
        model.eval()
        _musicgen = (processor, model, dev)
        log(f"MusicGen ready in {time.time() - t0:.1f}s")
    return _musicgen


def _load_audioldm():
    global _audioldm
    if _audioldm is None:
        from diffusers import AudioLDMPipeline

        dev = _device()
        log(f"loading AudioLDM '{SFX_MODEL}' on {dev} (first run downloads weights)")
        t0 = time.time()
        pipe = AudioLDMPipeline.from_pretrained(SFX_MODEL).to(dev)
        _audioldm = (pipe, dev)
        log(f"AudioLDM ready in {time.time() - t0:.1f}s")
    return _audioldm


def _wav_bytes(samples, sample_rate: int) -> bytes:
    import numpy as np
    import soundfile as sf

    arr = np.asarray(samples, dtype=np.float32)
    if arr.ndim == 2 and arr.shape[0] < arr.shape[1]:
        arr = arr.T  # (channels, frames) -> (frames, channels)
    arr = np.nan_to_num(arr)
    peak = float(np.max(np.abs(arr))) if arr.size else 0.0
    if peak < SILENCE_PEAK:
        raise SilentOutput(f"model produced silent audio (peak={peak:.6f})")
    if peak > 0.99:
        arr = arr * (0.99 / peak)
    buf = io.BytesIO()
    sf.write(buf, arr, sample_rate, format="WAV", subtype="PCM_16")
    return buf.getvalue()


def _seed(seed):
    if seed is None:
        return
    import torch

    torch.manual_seed(int(seed))


def generate_musicgen(prompt: str, duration: float, seed=None) -> tuple[bytes, str]:
    import torch

    processor, model, dev = _load_musicgen()
    frame_rate = int(getattr(model.config.audio_encoder, "frame_rate", 50))
    sample_rate = int(model.config.audio_encoder.sampling_rate)
    max_new_tokens = max(8, int(duration * frame_rate))
    _seed(seed)
    inputs = processor(text=[prompt], padding=True, return_tensors="pt").to(dev)
    with torch.inference_mode():
        audio = model.generate(**inputs, do_sample=True, guidance_scale=3.0, max_new_tokens=max_new_tokens)
    samples = audio[0].float().cpu().numpy()  # (channels, frames)
    if samples.shape[0] == 1:
        samples = samples[0]
    return _wav_bytes(samples, sample_rate), f"musicgen:{MUSICGEN_MODEL}"


def generate_audioldm(prompt: str, duration: float, seed=None) -> tuple[bytes, str]:
    import torch

    pipe, dev = _load_audioldm()
    generator = None
    if seed is not None:
        generator = torch.Generator(device="cpu").manual_seed(int(seed))
    result = pipe(
        prompt,
        num_inference_steps=int(os.environ.get("DEX_SFX_STEPS", "50")),
        audio_length_in_s=float(duration),
        generator=generator,
    )
    samples = result.audios[0]
    return _wav_bytes(samples, 16000), f"audioldm:{SFX_MODEL}"


def _clamp(value, lo: float, hi: float, default: float) -> float:
    try:
        v = float(value)
    except (TypeError, ValueError):
        return default
    if v != v:  # NaN
        return default
    return max(lo, min(hi, v))


class Handler(BaseHTTPRequestHandler):
    server_version = "DexAudioGen/" + VERSION

    def log_message(self, fmt, *args):  # route access logs to stderr with prefix
        log(fmt % args)

    def _send_json(self, code: int, payload: dict) -> None:
        body = json.dumps(payload).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _send_wav(self, data: bytes, backend: str) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "audio/wav")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("X-Dex-Backend", backend)
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):  # noqa: N802
        if self.path.rstrip("/") in ("", "/health"):
            self._send_json(
                200,
                {
                    "status": "ok",
                    "service": "dex-audiogen",
                    "version": VERSION,
                    "music_model": MUSICGEN_MODEL,
                    "sfx_backend": "audioldm" if _use_audioldm() else "musicgen",
                    "sfx_model": SFX_MODEL if _use_audioldm() else MUSICGEN_MODEL,
                    "music_loaded": _musicgen is not None,
                    "sfx_loaded": _audioldm is not None,
                },
            )
            return
        self._send_json(404, {"error": f"unknown path {self.path}"})

    def do_POST(self):  # noqa: N802
        path = self.path.rstrip("/")
        if path not in ("/v1/audio/music", "/v1/audio/sfx"):
            self._send_json(404, {"error": f"unknown path {self.path}"})
            return
        try:
            length = int(self.headers.get("Content-Length") or 0)
            raw = self.rfile.read(length) if length > 0 else b"{}"
            req = json.loads(raw.decode("utf-8") or "{}")
        except (ValueError, UnicodeDecodeError) as exc:
            self._send_json(400, {"error": f"invalid JSON body: {exc}"})
            return

        prompt = str(req.get("prompt") or "").strip()
        if not prompt:
            self._send_json(400, {"error": "'prompt' is required"})
            return
        seed = req.get("seed")

        try:
            with _lock:
                t0 = time.time()
                if path == "/v1/audio/music":
                    duration = _clamp(req.get("duration_secs"), 2.0, MAX_MUSIC_SECS, 15.0)
                    lyrics = str(req.get("lyrics") or "").strip()
                    if lyrics:
                        log("MusicGen is instrumental-only; lyrics are used as mood context, not sung")
                        prompt = f"{prompt}. Mood/theme inspired by: {lyrics[:300]}"
                    data, backend = generate_musicgen(prompt, duration, seed)
                else:
                    duration = _clamp(req.get("duration_secs"), 0.5, MAX_SFX_SECS, 4.0)
                    sfx_prompt = f"sound effect, foley, no music: {prompt}"
                    if _use_audioldm():
                        try:
                            data, backend = generate_audioldm(prompt, duration, seed)
                        except SilentOutput as exc:
                            log(f"AudioLDM {exc}; falling back to MusicGen")
                            data, backend = generate_musicgen(sfx_prompt, duration, seed)
                    else:
                        data, backend = generate_musicgen(sfx_prompt, duration, seed)
                log(f"{path} done in {time.time() - t0:.1f}s via {backend} ({len(data)} bytes)")
        except Exception as exc:  # noqa: BLE001
            traceback.print_exc(file=sys.stderr)
            self._send_json(500, {"error": f"{type(exc).__name__}: {exc}"})
            return
        self._send_wav(data, backend)


def main() -> None:
    parser = argparse.ArgumentParser(description="Dex local music/SFX generation sidecar")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8189)
    parser.add_argument("--preload", action="store_true", help="Load MusicGen before serving")
    args = parser.parse_args()
    if args.preload:
        _load_musicgen()
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    log(f"listening on http://{args.host}:{args.port} (music={MUSICGEN_MODEL})")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
