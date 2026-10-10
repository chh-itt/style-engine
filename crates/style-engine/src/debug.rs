//! Debug tooling (F3e, ADR-0027): host-side human-readable views.
//!
//! Complementary to the F3a2 serde dumps (`paint_dump`, the machine
//! round-trip channel): this module produces **human-readable** organized
//! text — filtered, sorted, indented — for diagnosing "why is it painted
//! wrong / why is the layout wrong". Everything reuses `Debug` derive output:
//! when neutral enums such as `PaintOp` gain variants, the exhaustive matches
//! in this module are forced to update by the compiler (the same discipline
//! as F3a2's "no wildcard arms within the crate"), so the wording needs no
//! duplicate maintenance.
//!
//! Three views (ADR-0027 D1):
//! 1. `display_list_dump` — the DisplayList tree view (Push/Pop
//!    indentation);
//! 2. [`ComputedStyle::debug_dump`](crate::computed::ComputedStyle::debug_dump)
//!    — explicitly materialized slots + custom properties (implemented in
//!    computed.rs, cohesive with the slot table);
//! 3. [`crate::StyleEngine::layout_tree_dump`] /
//!    [`crate::engine::Frame::boxes_dump`] — two separate views: the
//!    structure tree and the geometry boxes.

use crate::paint::{DisplayList, PaintOp};

/// Human-readable DisplayList view: one op per line (`Debug` output),
/// indented one level after each `Push*` and outdented one level before each
/// `Pop*`, forming a tree view; the first line summarizes the op count and
/// the frame generation number.
///
/// The indentation semantics align with `paint_node`'s stacking state
/// machine: pushing clip/opacity/transform/scroll state opens a child scope.
/// Op indices (line prefixes) let hosts correlate against
/// [`crate::paint::HitTestHit`] and locate positions in recorded replays.
pub fn display_list_dump(dl: &DisplayList) -> String {
    let mut out = format!(
        "DisplayList ops={} generation={}\n",
        dl.ops.len(),
        dl.generation
    );
    let mut depth: usize = 0;
    for (i, op) in dl.ops.iter().enumerate() {
        if matches!(
            op,
            PaintOp::PopClip | PaintOp::PopOpacity | PaintOp::PopTransform | PaintOp::PopScroll
        ) {
            depth = depth.saturating_sub(1);
        }
        out.push_str(&"  ".repeat(depth));
        out.push_str(&format!("[{i:03}] {op:?}\n"));
        if matches!(
            op,
            PaintOp::PushClip { .. }
                | PaintOp::PushClipPath { .. }
                | PaintOp::PushOpacity { .. }
                | PaintOp::PushTransform { .. }
                | PaintOp::PushScroll { .. }
        ) {
            depth += 1;
        }
    }
    out
}

// [`ComputedStyle::debug_dump`]（computed.rs 内聚实现，私有槽位表同类可
// 及）：显式物化槽位按 `PropertyId::ALL` 序 `css_name: {Debug}` 一行一个
// （None 槽位不出现 = 未显式出现在级联中，语义即初始/继承缺省），随后
// custom properties（`--name: value` 按名字典序）、字体度量、伪元素标记。
