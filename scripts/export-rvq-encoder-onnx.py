#!/usr/bin/env python3
"""Export the community RVQ encoder to ONNX, so the studio can run it.

MiniMax published Music3 without the encoder that turns audio into the codes
the model itself generates. Without it a finished track cannot be handed back:
no continuing a song, no replacing a chorus, no writing an intro in front of
one. The weights were reconstructed by the community; this turns them into an
ONNX graph, which the studio already knows how to run - ONNX Runtime is here
for stem separation and for Parakeet.

That choice is deliberate. The alternative was a second implementation of the
network in C++ inside the engine, which means forking the engine, carrying a
patch against someone else's repository for ever, and writing convolutions and
attention a second time. The studio can already run ONNX on the card or the
processor, and this network is small.

The definition below is the reference implementation published with the
weights (SimpleTuner/open-rvq-encoder-minimax-music3), narrowed to inference:
dropout is gone, and the depth decoder's greedy chain is written out because
ONNX has no room for Python control flow.

Usage:
    python scripts/export-rvq-encoder-onnx.py rvq_encoder.safetensors rvq-encoder.onnx
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import torch
import torch.nn as nn
import torch.nn.functional as F
from safetensors.torch import load_file


class ResBlock(nn.Module):
    """GroupNorm(1) + dilated conv + point conv, added back to the input."""

    def __init__(self, dim: int, dilation: int):
        super().__init__()
        self.norm = nn.GroupNorm(1, dim)
        self.conv1 = nn.Conv1d(dim, dim, 3, padding=dilation, dilation=dilation)
        self.conv2 = nn.Conv1d(dim, dim, 1)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return x + self.conv2(F.gelu(self.conv1(F.gelu(self.norm(x)))))


class EncoderLayer(nn.Module):
    """Pre-norm attention and feed-forward, with muP's attention scale."""

    def __init__(self, d_model: int, heads: int, ff_mult: int, attention_multiplier: float):
        super().__init__()
        self.heads = heads
        self.head_dim = d_model // heads
        self.scale = attention_multiplier / self.head_dim
        self.norm1 = nn.LayerNorm(d_model)
        self.norm2 = nn.LayerNorm(d_model)
        self.q_proj = nn.Linear(d_model, d_model)
        self.k_proj = nn.Linear(d_model, d_model)
        self.v_proj = nn.Linear(d_model, d_model)
        self.out_proj = nn.Linear(d_model, d_model)
        self.linear1 = nn.Linear(d_model, d_model * ff_mult)
        self.linear2 = nn.Linear(d_model * ff_mult, d_model)

    def _split(self, x: torch.Tensor) -> torch.Tensor:
        batch, frames, _ = x.shape
        return x.view(batch, frames, self.heads, self.head_dim).transpose(1, 2)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        normalized = self.norm1(x)
        query, key, value = self._split(self.q_proj(normalized)), self._split(self.k_proj(normalized)), self._split(self.v_proj(normalized))
        scores = torch.matmul(query, key.transpose(-2, -1)) * self.scale
        attended = torch.matmul(F.softmax(scores, dim=-1), value).transpose(1, 2).reshape(x.shape)
        x = x + self.out_proj(attended)
        return x + self.linear2(F.gelu(self.linear1(self.norm2(x))))


class DepthLayer(nn.Module):
    def __init__(self, dim: int, heads: int, ff_mult: int, attention_multiplier: float):
        super().__init__()
        self.heads = heads
        self.head_dim = dim // heads
        self.scale = attention_multiplier / self.head_dim
        self.norm1 = nn.LayerNorm(dim)
        self.norm2 = nn.LayerNorm(dim)
        self.q_proj = nn.Linear(dim, dim)
        self.k_proj = nn.Linear(dim, dim)
        self.v_proj = nn.Linear(dim, dim)
        self.out_proj = nn.Linear(dim, dim)
        self.linear1 = nn.Linear(dim, dim * ff_mult)
        self.linear2 = nn.Linear(dim * ff_mult, dim)

    def _split(self, x: torch.Tensor) -> torch.Tensor:
        batch, steps, _ = x.shape
        return x.view(batch, steps, self.heads, self.head_dim).transpose(1, 2)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        normalized = self.norm1(x)
        query, key, value = self._split(self.q_proj(normalized)), self._split(self.k_proj(normalized)), self._split(self.v_proj(normalized))
        scores = torch.matmul(query, key.transpose(-2, -1)) * self.scale
        attended = torch.matmul(F.softmax(scores, dim=-1), value).transpose(1, 2).reshape(x.shape)
        x = x + self.out_proj(attended)
        return x + self.linear2(F.gelu(self.linear1(self.norm2(x))))


class Encoder(nn.Module):
    def __init__(self, config: dict):
        super().__init__()
        d_model = config["d_model"]
        vocabs = config["codebook_vocab_sizes"]
        self.conv_in = nn.Conv1d(config["latent_channels"], d_model, 7, padding=3)
        self.blocks = nn.ModuleList(ResBlock(d_model, dilation) for dilation in config["conv_dilations"])
        self.position = nn.Parameter(torch.zeros(1, config["max_position_embeddings"], d_model))
        self.transformer = nn.ModuleList(
            EncoderLayer(d_model, config["num_heads"], config["ff_mult"], config["mup_attention_multiplier"])
            for _ in range(config["num_layers"])
        )
        self.norm_out = nn.LayerNorm(d_model)
        self.heads = nn.ModuleList([nn.Linear(d_model, vocabs[0])])

        depth_dim = config["depth_decoder_dim"]
        self.depth_decoder = nn.Module()
        self.depth_decoder.context_projection = nn.Linear(d_model, depth_dim, bias=False)
        self.depth_decoder.prior_embeddings = nn.ModuleList(nn.Embedding(size, depth_dim) for size in vocabs[:-1])
        self.depth_decoder.position = nn.Parameter(torch.zeros(1, len(vocabs), depth_dim))
        self.depth_decoder.layers = nn.ModuleList(
            DepthLayer(depth_dim, config["depth_decoder_heads"], config["depth_decoder_ff_mult"], config["mup_attention_multiplier"])
            for _ in range(config["depth_decoder_layers"])
        )
        self.depth_decoder.norm = nn.LayerNorm(depth_dim)
        self.depth_decoder.heads = nn.ModuleList(nn.Linear(depth_dim, size) for size in vocabs[1:])

    def _depth_step(self, sequence: torch.Tensor) -> torch.Tensor:
        hidden = sequence + self.depth_decoder.position[:, : sequence.shape[1]]
        for layer in self.depth_decoder.layers:
            hidden = layer(hidden)
        return self.depth_decoder.norm(hidden)

    def forward(self, latents: torch.Tensor, pool: torch.Tensor) -> torch.Tensor:
        """latents [B, T, 128] at 86.13 Hz, pool [B, F, T] onto the 25 Hz grid.

        Returns the eight codes per frame as [B, F, 8]: the semantic code first,
        then the seven acoustic ones, each the argmax of its head - the same
        greedy chain the reference takes.
        """
        hidden = self.conv_in(latents.transpose(1, 2))
        for block in self.blocks:
            hidden = block(hidden)
        hidden = torch.bmm(pool, hidden.transpose(1, 2))
        hidden = hidden + self.position[:, : pool.shape[1]]
        for layer in self.transformer:
            hidden = layer(hidden)
        hidden = self.norm_out(hidden)

        semantic = self.heads[0](hidden).argmax(dim=-1)
        batch, frames, _ = hidden.shape
        sequence = torch.cat(
            (
                self.depth_decoder.context_projection(hidden).reshape(batch * frames, 1, -1),
                self.depth_decoder.prior_embeddings[0](semantic).reshape(batch * frames, 1, -1),
            ),
            dim=1,
        )
        codes = [semantic]
        for index, head in enumerate(self.depth_decoder.heads):
            selected = head(self._depth_step(sequence)[:, -1]).argmax(dim=-1)
            codes.append(selected.view(batch, frames))
            if index + 1 < len(self.depth_decoder.heads):
                prior = self.depth_decoder.prior_embeddings[index + 1](selected).reshape(batch * frames, 1, -1)
                sequence = torch.cat((sequence, prior), dim=1)
        return torch.stack(codes, dim=-1)


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    source, destination = Path(sys.argv[1]), Path(sys.argv[2])
    config = json.loads(source.with_name("rvq_encoder_config.json").read_text(encoding="utf-8"))

    model = Encoder(config)
    missing, unexpected = model.load_state_dict(load_file(str(source)), strict=False)
    # Every weight has to land: a silently skipped tensor is a network that
    # runs and returns nonsense.
    if missing or unexpected:
        raise SystemExit(f"weights did not match the definition\n  missing: {missing[:6]}\n  unexpected: {unexpected[:6]}")
    model.eval()

    frames, latent_length = 16, 55
    latents = torch.randn(1, latent_length, config["latent_channels"])
    pool = torch.zeros(1, frames, latent_length)
    for frame in range(frames):
        span = slice(frame * latent_length // frames, (frame + 1) * latent_length // frames)
        pool[0, frame, span] = 1.0 / max(1, span.stop - span.start)

    with torch.no_grad():
        torch.onnx.export(
            model,
            (latents, pool),
            str(destination),
            input_names=["latents", "pool"],
            output_names=["codes"],
            dynamic_axes={"latents": {1: "latent_frames"}, "pool": {1: "frames", 2: "latent_frames"}, "codes": {1: "frames"}},
            opset_version=17,
        )
    print(f"{destination}: {destination.stat().st_size / 1e6:.1f} MB")


if __name__ == "__main__":
    main()
