//! 调试工具链（F3e，ADR-0027）：宿主侧人读视图。
//!
//! 与 F3a2 serde dump（`paint_dump`，机器往返通道）互补：本模块产出
//! **人类可读**的组织化文本——过滤、排序、缩进——供排查「为什么画错/
//! 为什么布局不对」。全部复用 `Debug` derive 输出：`PaintOp` 等中立枚举
//! 新增变体时本模块的穷尽 match 会被编译器强制更新（与 F3a2「同 crate
//! 删通配臂」同一纪律），措辞零重复维护。
//!
//! 三视图（ADR-0027 D1）：
//! 1. `display_list_dump` —— DisplayList 树视图（Push/Pop 缩进）；
//! 2. [`ComputedStyle::debug_dump`](crate::computed::ComputedStyle::debug_dump)
//!    —— 显式物化槽位 + custom properties（computed.rs，同类内聚）；
//! 3. [`crate::StyleEngine::layout_tree_dump`] /
//!    [`crate::engine::Frame::boxes_dump`] —— 结构树与几何盒两分离视图。

use crate::paint::{DisplayList, PaintOp};

/// DisplayList 人读视图：每 op 一行（`Debug` 输出），`Push*` 后缩进一级、
/// `Pop*` 前反缩进一级，形成树视图；首行汇总 op 数与帧生成号。
///
/// 缩进语义与 `paint_node` 的层叠状态机对齐：clip/opacity/transform/scroll
/// 四类状态推入即开启子作用域。op 序号（行前缀）供宿主对照
/// [`crate::paint::HitTestHit`] 与录制回放定位。
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
