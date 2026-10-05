//! F2（ADR-0022 D2）text-overflow: ellipsis 锁测试：截断+省略号、
//! 不溢出原样、overflow visible 不截（spec：仅裁剪语境生效）。
//!
//! 语义（ADR-0022 D2）：父（包含块）overflow 非 visible ∧ 父
//! text-overflow=ellipsis ∧ 叶 nowrap ∧ 测量宽>包含块内容宽 → 二分
//! 最长字符前缀使 width(prefix)+"…" ≤ avail → 绘制文本替换（盒几何
//! 不变；"…" 字形吃基色）。

#![cfg(feature = "text")]

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

const FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../style-engine-demo/assets/fonts/DejaVuSans.ttf"
));

/// 骨架：#root(1) 块容器（300px）→ 单文本叶(2)。
fn engine_ellipsis(sheet: &str, text: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.add_font(FONT.to_vec());
    engine.set_stylesheet(sheet);
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut n = StyleNode::default();
    n.id = Some("t".to_string());
    n.text = Some(text.to_string());
    engine.insert(Some(1), 2, n).unwrap();
    engine
}

/// 叶 Text op 文本（首个 Text op）。
fn leaf_text(e: &mut StyleEngine<u64>) -> String {
    let fr = e.frame((300.0, 600.0), 1.0, 0.0);
    for op in &fr.paint.ops {
        if let style_engine::paint::PaintOp::Text { text, .. } = op {
            return text.clone();
        }
    }
    panic!("no Text op");
}

/// 80 字符文本，16px DejaVu nowrap ≈ 700px > 300px 容器。
const LONG: &str = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore";

#[test]
fn ellipsis_truncates_with_ellipsis_char() {
    let mut e = engine_ellipsis(
        "#root { width: 300px; overflow: hidden; text-overflow: ellipsis; } #t { font-size: 16px; font-family: \"DejaVu Sans\"; white-space: nowrap; }",
        LONG,
    );
    let t = leaf_text(&mut e);
    assert!(t.ends_with('\u{2026}'), "截断文本以…收尾：{t:?}");
    assert_ne!(t, LONG, "发生截断");
    let prefix = &t[..t.len() - 3];
    assert!(LONG.starts_with(prefix), "截断=原文本前缀");
}

#[test]
fn ellipsis_not_applied_when_fits() {
    let mut e = engine_ellipsis(
        "#root { width: 300px; overflow: hidden; text-overflow: ellipsis; } #t { font-size: 16px; font-family: \"DejaVu Sans\"; white-space: nowrap; }",
        "hello",
    );
    let t = leaf_text(&mut e);
    assert_eq!(t, "hello", "不溢出 → 原样（无省略号）");
}

#[test]
fn ellipsis_not_applied_when_overflow_visible() {
    let mut e = engine_ellipsis(
        "#root { width: 300px; text-overflow: ellipsis; } #t { font-size: 16px; font-family: \"DejaVu Sans\"; white-space: nowrap; }",
        LONG,
    );
    let t = leaf_text(&mut e);
    assert_eq!(t, LONG, "overflow visible → 不截（spec：仅裁剪语境生效）");
}

// ===== F2（ADR-0022 D3）：-webkit-line-clamp =====

#[test]
fn line_clamp_truncates_to_two_lines() {
    // 折行叶（white-space normal）：长文在 300px 容器折多行，clamp 2 →
    // 截断至 2 行并以…收尾。
    let mut e = engine_ellipsis(
        "#root { width: 300px; overflow: hidden; -webkit-line-clamp: 2; } #t { font-size: 16px; font-family: \"DejaVu Sans\"; }",
        LONG,
    );
    let t = leaf_text(&mut e);
    assert!(t.ends_with('\u{2026}'), "钳 2 行文本以…收尾：{t:?}");
    assert_ne!(t, LONG, "发生截断");
    let prefix = &t[..t.len() - 3];
    assert!(LONG.starts_with(prefix), "截断=原文本前缀");
}

#[test]
fn line_clamp_none_keeps_full_text() {
    let mut e = engine_ellipsis(
        "#root { width: 300px; overflow: hidden; } #t { font-size: 16px; font-family: \"DejaVu Sans\"; }",
        LONG,
    );
    let t = leaf_text(&mut e);
    assert_eq!(t, LONG, "无 clamp → 原样");
}

#[test]
fn line_clamp_not_applied_when_fits() {
    let mut e = engine_ellipsis(
        "#root { width: 300px; overflow: hidden; -webkit-line-clamp: 2; } #t { font-size: 16px; font-family: \"DejaVu Sans\"; }",
        "hello",
    );
    let t = leaf_text(&mut e);
    assert_eq!(t, "hello", "单行不超 2 行 → 原样");
}
