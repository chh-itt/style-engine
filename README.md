# style-engine

**A framework-agnostic CSS semantic layer for native Rust GUIs**: it owns the full chain from style declarations → cascade → layout → paint commands, and leaves every side effect (windows, events, clocks, resources) to the host.

- **L1 Style algebra**: real CSS text parsing (cssparser) + selector matching (selectors) + an in-house property grammar, producing `ComputedStyle` (183 property slots + custom properties).
- **L2 Layout** (feature = `layout`): taffy (flex/grid/block/absolute) + an in-house resolution layer (calc across 17 axes as settled expressions, two-phase table column sizing, multi-column bisection balancing) + the built-in text stack parley (measurement / line breaking / bidi).
- **L3 Paint**: a neutral `DisplayList` (a sequence of `PaintOp` commands) — rendering is up to whichever backend the host plugs in; reference GPU sink `style-engine-vello`, deterministic ground-truth sink `style-engine-soft` (zero-dependency pure software rasterizer), lightweight CPU backend `style-engine-tiny` (tiny-skia, for embedded / no-GPU targets).

**Positioning**: a CSS semantic layer for desktop GUIs (aimed at egui/iced/bevy-style hosts); the browser is the **yardstick**, not the target — correctness is proven by the conformance harness against Chromium goldens (dual channel: Numeric with 0.5px tolerance + Pixel with error classification; currently 35 cases, zero xfail). The subset boundary is defined authoritatively by [docs/FEATURES.md](docs/FEATURES.md): every feature line either carries golden evidence or an explicit exclusion reason with a revisit condition.

## Quick start

Install from crates.io:

```powershell
cargo add style-engine style-engine-vello
```

```rust
use style_engine::{StyleEngine, StyleNode};

let mut engine = StyleEngine::new();
engine.set_stylesheet(".btn { background-color: #3366cc; padding: 8px 16px; }");

// Host tree mirror: root 1 → button 2 (K is the host's own Copy + Eq + Hash key)
engine.insert(None, 1, StyleNode::default())?;
let mut btn = StyleNode::default();
btn.name = Some("button".into());
btn.classes = ["btn".to_string()].into_iter().collect();
engine.insert(Some(1), 2, btn)?;

// Per frame: viewport + scale factor + monotonic clock (the crate has no internal clock; same input, same output)
let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
let hit = frame.find(2).expect("node laid out");

// Paint: hand the DisplayList to any sink
let scene = style_engine_vello::render(&frame.paint);
```

Side-effect-free contract: font/image bytes are pushed in by the host (`add_font` / `add_image`), interaction state is pushed by the host (`set_state`), and scroll offsets live in the host (`set_scroll_offset`; ranges are reported via `Frame.scrollable`). Errors come in two tracks: CSS content errors are tolerated per spec into `ParseReport`; host contract violations surface as `ContractError`.

## Workspace layout

| crate | role |
|---|---|
| `style-engine` | Core: L1/L2/L3, no GPU dependency |
| `style-engine-vello` | GPU sink reference implementation (wgpu/vello live only here) |
| `style-engine-soft` | Pure software rasterizer sink (zero third-party runtime deps, pixel-deterministic) |
| `style-engine-tiny` | Lightweight CPU sink (tiny-skia/parley, for embedded / no-GPU targets) |
| `style-engine-conformance` | Chromium golden dual-channel comparison harness |
| `style-engine-demo` | winit adversarial-consumer demo |

## Feature gates

- `layout` (on by default): taffy layout and all resolution passes
- `text` (on by default): parley text measurement / line breaking
- `serde`: machine channel for DisplayList/ComputedStyle (paint_dump OpDump mirror, debug dump round-trip locks)
- `--no-default-features`: only CSS parsing / cascade / paint compile

## Documentation index

The referenced documents are maintained in Chinese:

- [CONTEXT.md](CONTEXT.md) — domain language (glossary)
- [docs/V1-SCOPE.md](docs/V1-SCOPE.md) — v1.0 behavioral acceptance baseline (promises / deviations / exclusions)
- [docs/adr/](docs/adr/) — architecture decision records (DisplayList neutral contract, rendering stack, conformance, push-based sync, scrolling, layering, transform, …)
- [docs/FEATURES.md](docs/FEATURES.md) — feature registry (authoritative subset boundary + A/B/C deviation tiers)
- [docs/PERFORMANCE.md](docs/PERFORMANCE.md) — performance budgets and gate derivations
- [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md) — dependency policy and version-floor chain
- [CHANGELOG.md](CHANGELOG.md) — phase-by-phase history

## Local development

```powershell
.\run.ps1            # fmt + clippy + test --all-features + hack powerset (+ perf)
cargo test -p style-engine --all-features   # core tests only
```

MSRV **1.90** (edition 2024; see DEPENDENCIES.md for the dependency floor chain). Licensed under MIT OR Apache-2.0; published to crates.io (2026-10 decision, superseding the earlier "do not publish" policy).
