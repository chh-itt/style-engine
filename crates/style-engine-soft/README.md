# style-engine-soft

[![crates.io](https://img.shields.io/crates/v/style-engine-soft.svg)](https://crates.io/crates/style-engine-soft)

A pure-software paint sink (the second sink) for the [`style-engine`](https://crates.io/crates/style-engine) DisplayList — **zero GPU, zero third-party dependencies** (pure Rust standard-library rasterization).

## When to choose it

- **Ground-truth reference** for CI-grade pixel assertions: no GPU, no floating-point noise, byte-for-byte deterministic and reproducible;
- Verifying the sink-independence contract: the same `DisplayList` renders with consistent semantics across backends;
- Environments where a GPU stack cannot be introduced.

## Features

- All 20 `PaintOp` variants painted pixel-by-pixel: blend 18/18, full filter-function pipeline, per-pixel borders;
- Compositing semantics: sRGB-encoded values composited directly src-over (consistent with vello/Chromium defaults), gradient stops interpolated in sRGB (the CSS default interpolation space);
- The filter/blur/blend infrastructure (`filter.rs` / `blur_alpha_u8` / `blend_pixel`) is also the single source of truth for [`style-engine-tiny`](https://crates.io/crates/style-engine-tiny) (ADR-0043 D2), preventing two-implementation drift.

## Positioning

This crate is a contract-validation and ground-truth reference implementation, not an end-user-facing paint library. Publication positioning: [`docs/BREAKING-POLICY.md`](https://github.com/chh-itt/style-engine/blob/main/docs/BREAKING-POLICY.md); capability matrix: [`docs/SINK-MATRIX.md`](https://github.com/chh-itt/style-engine/blob/main/docs/SINK-MATRIX.md). For API details, rely on the crate documentation (`cargo doc`).
