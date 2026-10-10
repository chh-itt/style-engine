# style-engine-vello

A Vello (wgpu) **GPU paint sink** for the [`style-engine`](https://crates.io/crates/style-engine) DisplayList.

## When to choose it

- The target platform has a usable GPU adapter and you want GPU batched-render throughput and vello rendering quality;
- Text goes through parley (same line-breaking source as the engine's measurement), and all 20 `PaintOp` variants of the DisplayList are consumed.

## Usage

```rust
use style_engine::StyleEngine;
use style_engine_vello::render;

let mut engine: StyleEngine<u64> = StyleEngine::new();
engine.set_stylesheet("div { width: 64px; height: 64px; background-color: #f00 }");
engine.insert(None, 1, Default::default())?;

let frame = engine.frame((64.0, 64.0), 1.0, 0.0);
let scene = render(&frame.paint); // vello::Scene → consumed by the host's wgpu pipeline
```

Incremental path: `render_ops` / `render_ops_with_text` (the latter lands glyphs via `VelloTextSystem` — `render`/`render_ops` are the text-free variants and skip `Text` ops). Offscreen pixel readback: `render_offscreen` (returns `Ok(None)` when no adapter is available; the skip semantics are the caller's decision).

## Capability boundaries (recorded as Tier B)

Tier grading and the three-sink comparison live in the deviation matrix of [`docs/SINK-MATRIX.md`](../../docs/SINK-MATRIX.md):

- Non-opacity filter chains / `BackdropFilter`: warn-once degradation (upstream vello filter proper / backdrop primitive not yet landed);
- `PlusLighter` / `PlusDarker`: fall back to `Mix::Normal` (upstream peniko Mix lacks the two modes);
- box-shadow / text-shadow blur: multiple concentric-ring approximation (not a true Gaussian);
- conic gradient start>360° phase and radial ellipse rx/ry: pending upstream arbitration.

## Sibling crates

- [`style-engine-soft`](https://crates.io/crates/style-engine-soft): zero-dependency pure CPU reference (deterministic ground truth);
- [`style-engine-tiny`](https://crates.io/crates/style-engine-tiny): tiny-skia pure CPU publishable sink (embedded / no-GPU targets).
