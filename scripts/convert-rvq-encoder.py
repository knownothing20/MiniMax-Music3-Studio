#!/usr/bin/env python3
"""Turn the community RVQ encoder into a GGUF the engine can read.

MiniMax published Music3 without the encoder that turns audio into the codes
the model generates, so a finished track cannot be handed back to it: no
continuation, no replacing a chorus, no writing an intro in front of one. The
weights were reconstructed by the community (SimpleTuner/open-rvq-encoder-*),
but only as safetensors, and the engine reads GGUF.

This is the whole network, read from the checkpoint's own header rather than
assumed:

    latents [T, 128] at 86.13 Hz
      -> conv_in (kernel 7, 128 -> 1088)
      -> 6 residual blocks, dilations 1/3/9, each conv(k3) + conv(k1) + norm
      -> pooling onto the 25 Hz frame grid
      -> 8 transformer layers, 17 heads, d_model 1088, ff 4352
      -> heads.0 -> the semantic code, one of 16384
      -> depth decoder (2 layers, d 512) -> the 7 acoustic codes

Nothing is quantised here: 169M parameters in f32 is 676 MB, which is small
next to the sets the studio already downloads, and a first implementation
should not have quantisation error on top of everything else.

Usage:
    python scripts/convert-rvq-encoder.py rvq_encoder.safetensors rvq-encoder.gguf
"""

from __future__ import annotations

import json
import struct
import sys
from pathlib import Path

GGUF_MAGIC = 0x46554747
GGUF_VERSION = 3
GGML_TYPE_F32 = 0

# GGUF metadata value types, only the ones this writer emits.
UINT32, FLOAT32, STRING, ARRAY = 4, 6, 8, 9


def read_safetensors_header(path: Path) -> tuple[dict, int]:
    with path.open("rb") as handle:
        length = struct.unpack("<Q", handle.read(8))[0]
        header = json.loads(handle.read(length))
    return header, 8 + length


def write_string(out, text: str) -> None:
    data = text.encode("utf-8")
    out.write(struct.pack("<Q", len(data)))
    out.write(data)


def write_kv_uint32(out, key: str, value: int) -> None:
    write_string(out, key)
    out.write(struct.pack("<I", UINT32))
    out.write(struct.pack("<I", value))


def write_kv_string(out, key: str, value: str) -> None:
    write_string(out, key)
    out.write(struct.pack("<I", STRING))
    write_string(out, value)


def convert(source: Path, destination: Path, config: dict) -> None:
    header, data_start = read_safetensors_header(source)
    tensors = {name: meta for name, meta in header.items() if name != "__metadata__"}
    for name, meta in tensors.items():
        if meta["dtype"] != "F32":
            raise SystemExit(f"{name} is {meta['dtype']}; this converter expects F32")

    metadata = {
        "general.architecture": "rvq-encoder",
        "general.name": "open-rvq-encoder-minimax-music3",
    }
    numbers = {
        "rvq.latent_channels": config["latent_channels"],
        "rvq.d_model": config["d_model"],
        "rvq.num_layers": config["num_layers"],
        "rvq.num_heads": config["num_heads"],
        "rvq.ff_mult": config["ff_mult"],
        "rvq.max_position_embeddings": config["max_position_embeddings"],
        "rvq.depth_decoder_dim": config["depth_decoder_dim"],
        "rvq.depth_decoder_layers": config["depth_decoder_layers"],
        "rvq.depth_decoder_heads": config["depth_decoder_heads"],
        "rvq.semantic_vocab": config["codebook_vocab_sizes"][0],
        "rvq.acoustic_vocab": config["codebook_vocab_sizes"][1],
        "rvq.acoustic_codebooks": len(config["codebook_vocab_sizes"]) - 1,
    }

    with destination.open("wb") as out:
        out.write(struct.pack("<I", GGUF_MAGIC))
        out.write(struct.pack("<I", GGUF_VERSION))
        out.write(struct.pack("<Q", len(tensors)))
        out.write(struct.pack("<Q", len(metadata) + len(numbers)))
        for key, value in metadata.items():
            write_kv_string(out, key, value)
        for key, value in numbers.items():
            write_kv_uint32(out, key, int(value))

        # Tensor descriptors. GGUF states dimensions fastest-first, which is the
        # reverse of PyTorch's row-major shape.
        offset = 0
        placement: list[tuple[str, int, int]] = []
        for name in tensors:
            meta = tensors[name]
            shape = list(reversed(meta["shape"]))
            write_string(out, name)
            out.write(struct.pack("<I", len(shape)))
            for dimension in shape:
                out.write(struct.pack("<Q", dimension))
            out.write(struct.pack("<I", GGML_TYPE_F32))
            out.write(struct.pack("<Q", offset))
            size = 4
            for dimension in shape:
                size *= dimension
            placement.append((name, offset, size))
            offset += (size + 31) // 32 * 32  # the alignment GGUF defaults to

        while out.tell() % 32:
            out.write(b"\0")
        base = out.tell()

        with source.open("rb") as handle:
            for name, want_offset, size in placement:
                out.seek(base + want_offset)
                start, end = tensors[name]["data_offsets"]
                if end - start != size:
                    raise SystemExit(f"{name}: header says {end - start} bytes, shape says {size}")
                handle.seek(data_start + start)
                remaining = size
                while remaining:
                    chunk = handle.read(min(1 << 22, remaining))
                    if not chunk:
                        raise SystemExit(f"{name}: the checkpoint ended early")
                    out.write(chunk)
                    remaining -= len(chunk)

    print(f"{destination}: {len(tensors)} tensors, {destination.stat().st_size / 1e6:.1f} MB")


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    source = Path(sys.argv[1])
    destination = Path(sys.argv[2])
    config_path = source.with_name("rvq_encoder_config.json")
    if not config_path.is_file():
        raise SystemExit(f"the checkpoint's config is missing: {config_path}")
    convert(source, destination, json.loads(config_path.read_text(encoding="utf-8")))


if __name__ == "__main__":
    main()
