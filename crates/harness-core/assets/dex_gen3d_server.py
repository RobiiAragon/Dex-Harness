#!/usr/bin/env python3
"""Dex local text/image-to-3D sidecar.

Managed by Dex-Harness (`harness-3d`). It is written to
`~/.dex/runtimes/gen3d/dex_gen3d_server.py` and runs inside the managed
`~/.dex/runtimes/mlx-venv` interpreter, using only the standard library plus the packages
installed by the `gen3d` runtime (torch, diffusers, transformers, accelerate, trimesh, pillow,
fast-simplification, scipy).

Endpoints (bound to 127.0.0.1):
  GET  /health            -> JSON status (always fast; models load lazily)
  POST /v1/3d/generate    -> model/gltf-binary
       JSON       {"prompt", "seed"?, "steps"?, "guidance"?}            text -> 3D (Shap-E)
       multipart  image=<file> [prompt] [seed] [steps] [guidance]       image -> 3D (Shap-E img2img)

Models (Hugging Face, open, no token): DEX_SHAPE_MODEL (default openai/shap-e) and
DEX_SHAPE_IMG2IMG_MODEL (default openai/shap-e-img2img, downloaded only on first image request).
Meshes carry per-vertex colors and are rotated to glTF's Y-up convention.
"""

from __future__ import annotations

import argparse
import email.parser
import email.policy
import io
import json
import os
import sys
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

VERSION = "1"
TEXT_MODEL = os.environ.get("DEX_SHAPE_MODEL", "openai/shap-e")
IMAGE_MODEL = os.environ.get("DEX_SHAPE_IMG2IMG_MODEL", "openai/shap-e-img2img")
MAX_BODY = 32 * 1024 * 1024
# Real-time friendly budget (Dex flags > 50k triangles as not game-ready).
MAX_FACES = int(os.environ.get("DEX_GEN3D_MAX_FACES", "40000"))

_lock = threading.Lock()
_pipes: dict[str, object] = {}


def log(msg: str) -> None:
    print(f"[dex-gen3d] {msg}", file=sys.stderr, flush=True)


def _device() -> str:
    import torch

    if torch.backends.mps.is_available():
        return "mps"
    if torch.cuda.is_available():
        return "cuda"
    return "cpu"


def _pipe(kind: str):
    if kind in _pipes:
        return _pipes[kind]
    import torch
    from diffusers import ShapEImg2ImgPipeline, ShapEPipeline

    device = _device()
    dtype = torch.float16 if device != "cpu" else torch.float32
    cls, model = (ShapEPipeline, TEXT_MODEL) if kind == "text" else (ShapEImg2ImgPipeline, IMAGE_MODEL)
    t0 = time.time()
    log(f"loading {model} on {device} (first run downloads the weights)")
    pipe = cls.from_pretrained(model, torch_dtype=dtype).to(device)
    log(f"loaded {model} in {time.time() - t0:.1f}s")
    _pipes[kind] = pipe
    return pipe


def _to_glb(mesh) -> bytes:
    import numpy as np
    import trimesh

    verts = mesh.verts.detach().float().cpu().numpy()
    faces = mesh.faces.detach().cpu().numpy().astype("int64")
    if len(verts) == 0 or len(faces) == 0:
        raise RuntimeError("Shap-E produced an empty mesh; try a more concrete prompt")
    colors = None
    channels = getattr(mesh, "vertex_channels", None) or {}
    if all(k in channels for k in ("R", "G", "B")):
        rgb = np.stack([channels[k].detach().float().cpu().numpy() for k in ("R", "G", "B")], axis=1)
        alpha = np.ones((rgb.shape[0], 1))
        colors = (np.clip(np.concatenate([rgb, alpha], axis=1), 0.0, 1.0) * 255).astype("uint8")
    tm = trimesh.Trimesh(vertices=verts, faces=faces, vertex_colors=colors, process=True)
    # Shap-E is Z-up; glTF is Y-up.
    tm.apply_transform(trimesh.transformations.rotation_matrix(-np.pi / 2, [1, 0, 0]))
    tm.apply_translation(-tm.bounds.mean(axis=0))
    if len(tm.faces) > MAX_FACES:
        try:  # needs `fast-simplification`; keep the full mesh if it is unavailable
            full = tm
            tm = tm.simplify_quadric_decimation(face_count=MAX_FACES)
            if colors is not None:
                # Decimation drops vertex colors: copy each new vertex's color from the
                # nearest original vertex.
                from scipy.spatial import cKDTree

                _, idx = cKDTree(full.vertices).query(tm.vertices)
                tm.visual.vertex_colors = full.visual.vertex_colors[idx]
        except Exception as exc:  # noqa: BLE001
            log(f"decimation skipped: {exc}")
            tm = full
    data = tm.export(file_type="glb", include_normals=True)
    return bytes(data)


def generate(prompt: str, image_bytes: bytes | None, seed, steps: int, guidance: float) -> tuple[bytes, str]:
    import torch

    kind = "image" if image_bytes else "text"
    pipe = _pipe(kind)
    generator = None
    if seed is not None:
        try:
            generator = torch.Generator(device="cpu").manual_seed(int(seed))
        except (TypeError, ValueError):
            generator = None
    common = dict(
        guidance_scale=guidance,
        num_inference_steps=steps,
        frame_size=256,
        output_type="mesh",
        generator=generator,
    )
    if kind == "image":
        from PIL import Image

        img = Image.open(io.BytesIO(image_bytes)).convert("RGB").resize((256, 256))
        out = pipe(img, **common)
        backend = f"shap-e-img2img:{IMAGE_MODEL}"
    else:
        out = pipe(prompt, **common)
        backend = f"shap-e:{TEXT_MODEL}"
    return _to_glb(out.images[0]), backend


def _parse_multipart(content_type: str, body: bytes) -> dict:
    msg = email.parser.BytesParser(policy=email.policy.default).parsebytes(
        b"Content-Type: " + content_type.encode("latin-1") + b"\r\n\r\n" + body
    )
    fields: dict = {}
    for part in msg.iter_parts():
        name = part.get_param("name", header="content-disposition")
        if not name:
            continue
        payload = part.get_payload(decode=True) or b""
        fields[name] = payload if part.get_filename() else payload.decode("utf-8", "replace")
    return fields


def _num(value, cast, lo, hi, default):
    try:
        v = cast(value)
    except (TypeError, ValueError):
        return default
    return max(lo, min(hi, v))


class Handler(BaseHTTPRequestHandler):
    server_version = "DexGen3D/" + VERSION

    def log_message(self, fmt, *args):
        log(fmt % args)

    def _send_json(self, code: int, payload: dict) -> None:
        body = json.dumps(payload).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):  # noqa: N802
        if self.path.rstrip("/") in ("", "/health"):
            self._send_json(
                200,
                {
                    "status": "ok",
                    "service": "dex-gen3d",
                    "version": VERSION,
                    "text_model": TEXT_MODEL,
                    "image_model": IMAGE_MODEL,
                    "loaded": sorted(_pipes),
                },
            )
            return
        self._send_json(404, {"error": f"unknown path {self.path}"})

    def do_POST(self):  # noqa: N802
        if self.path.rstrip("/") != "/v1/3d/generate":
            self._send_json(404, {"error": f"unknown path {self.path}"})
            return
        length = int(self.headers.get("Content-Length") or 0)
        if length > MAX_BODY:
            self._send_json(413, {"error": "request too large (max 32 MB)"})
            return
        raw = self.rfile.read(length) if length > 0 else b""
        ctype = self.headers.get("Content-Type") or ""
        try:
            if ctype.startswith("multipart/form-data"):
                req = _parse_multipart(ctype, raw)
            else:
                req = json.loads(raw.decode("utf-8") or "{}")
        except (ValueError, UnicodeDecodeError) as exc:
            self._send_json(400, {"error": f"invalid request body: {exc}"})
            return

        image = req.get("image") if isinstance(req.get("image"), bytes) else None
        prompt = str(req.get("prompt") or "").strip()
        if not prompt and not image:
            self._send_json(400, {"error": "send a 'prompt' or an 'image'"})
            return
        steps = _num(req.get("steps"), int, 8, 128, 64)
        guidance = _num(req.get("guidance"), float, 1.0, 30.0, 15.0 if not image else 3.0)

        try:
            with _lock:
                t0 = time.time()
                data, backend = generate(prompt, image, req.get("seed"), steps, guidance)
                log(f"generated {len(data)} bytes via {backend} in {time.time() - t0:.1f}s")
        except Exception as exc:  # noqa: BLE001
            traceback.print_exc(file=sys.stderr)
            self._send_json(500, {"error": f"{type(exc).__name__}: {exc}"})
            return
        self.send_response(200)
        self.send_header("Content-Type", "model/gltf-binary")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("X-Dex-Backend", backend)
        self.end_headers()
        self.wfile.write(data)


def main() -> None:
    parser = argparse.ArgumentParser(description="Dex local text/image-to-3D sidecar (Shap-E)")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8198)
    parser.add_argument("--preload", action="store_true", help="Load the text-to-3D model before serving")
    args = parser.parse_args()
    if args.preload:
        _pipe("text")
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    log(f"listening on http://{args.host}:{args.port} (text={TEXT_MODEL})")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
