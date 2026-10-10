# style-engine-tiny

[![crates.io](https://img.shields.io/crates/v/style-engine-tiny.svg)](https://crates.io/crates/style-engine-tiny)

A tiny-skia **pure-CPU paint sink** (the third sink, P10, ADR-0043) for the [`style-engine`](https://crates.io/crates/style-engine) DisplayList.

## When to choose it

- Embedded / no-GPU targets that need a directly publishable CPU paint backend;
- Deterministic pixel regression on all platforms: the tiny leg of the conformance Pixel Channel has no GPU-probing logic and runs on every platform (including windows CI).

## Features

- tiny-skia 0.12 rasterizer (same as resvg) + skrifa 0.44 glyph outlines; text layout shares its source with the vello sink (parley; line breaking happens in L2);
- All 20 `PaintOp`; shadows / filters / text-shadow = true 3× box blur (reusing the single source of truth from [`style-engine-soft`](https://crates.io/crates/style-engine-soft), e.g. `blur_alpha_u8`, per ADR-0043 D2);
- Blending 18/18: the 16 standard modes go through tiny-skia's native pipeline, `PlusLighter` maps to `BlendMode::Plus`, and `PlusDarker` (missing in tiny-skia) goes through soft's per-pixel `blend_pixel`;
- Full support for font_features / variations / text decorations (incl. wavy) / text-shadow.

## Compositing invariants

tiny-skia 0.12 constraints (group mask size == SubPixmap, `Pattern` tile-local anchoring, integer y-boundary rows getting zero AA coverage) are recorded in the "tiny compositing invariants" section of [`docs/SINK-MATRIX.md`](https://github.com/chh-itt/style-engine/blob/main/docs/SINK-MATRIX.md) and locked by the built-in `tests/compositing.rs`. Capability boundaries and the three-sink deviation comparison live in the deviation matrix of the same file.
