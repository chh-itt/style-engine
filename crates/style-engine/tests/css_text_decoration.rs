//! F2（ADR-0022 D4/D5）text-decoration 与 text-shadow 锁测试：解析面
//! （简写/多关键字/多影）与绘制面（PaintOp::Text 携带 decorations/
//! shadows，颜色厚度已解析）。
//!
//! 语义（ADR-0022 D4/D5）：text-decoration=四长手（line 位集 1=underline
//! 2=overline 4=line-through；style；color 缺省 currentColor；thickness
//! auto=font_size/12 近似）+『line* | style | color | thickness』任意序
//! 简写；text-shadow=none | [<color>? dx dy blur? <color>?]#（继承）。

#![cfg(feature = "text")]

use style_engine::StyleEngine;
use style_engine::css::property::TextDecoStyleKind;
use style_engine::tree::StyleNode;

const FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../style-engine-demo/assets/fonts/DejaVuSans.ttf"
));

/// 骨架：#root(1) 块容器 → 单文本叶(2)。
fn engine_deco(sheet: &str, text: &str) -> StyleEngine<u64> {
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

/// 叶 Text op（克隆；decorations/shadows 字段断言用）。
fn text_op(e: &mut StyleEngine<u64>) -> style_engine::paint::PaintOp {
    let fr = e.frame((300.0, 600.0), 1.0, 0.0);
    for op in &fr.paint.ops {
        if let op @ style_engine::paint::PaintOp::Text { .. } = op {
            return op.clone();
        }
    }
    panic!("no Text op");
}

const HELLO: &str = "hello";

#[test]
fn decoration_shorthand_parses_and_carries() {
    let mut e = engine_deco(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; text-decoration: underline wavy red 2px; }",
        HELLO,
    );
    let op = text_op(&mut e);
    let style_engine::paint::PaintOp::Text { decorations, .. } = op else {
        panic!("not Text");
    };
    assert_eq!(decorations.len(), 1, "装饰产出一条");
    let d = &decorations[0];
    assert_eq!(d.line & 1, 1, "underline 位");
    assert_eq!(d.line & 6, 0, "无 overline/line-through");
    assert_eq!(d.style, TextDecoStyleKind::Wavy, "wavy 线型");
    assert!(
        (d.thickness_px - 2.0).abs() < 0.01,
        "thickness 2px 解析：{}",
        d.thickness_px
    );
    let [r, g, b, a] = d.color.components;
    assert!(
        r > 0.9 && g < 0.1 && b < 0.1 && a > 0.9,
        "red 解析：{r} {g} {b} {a}"
    );
}

#[test]
fn decoration_none_by_default() {
    let mut e = engine_deco(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; }",
        HELLO,
    );
    let op = text_op(&mut e);
    let style_engine::paint::PaintOp::Text { decorations, .. } = op else {
        panic!("not Text");
    };
    assert!(decorations.is_empty(), "无声明 → 无装饰");
}

#[test]
fn decoration_line_multiple_keywords() {
    let mut e = engine_deco(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; text-decoration-line: underline line-through; }",
        HELLO,
    );
    let op = text_op(&mut e);
    let style_engine::paint::PaintOp::Text { decorations, .. } = op else {
        panic!("not Text");
    };
    assert_eq!(decorations.len(), 1);
    assert_eq!(decorations[0].line, 1 | 4, "underline|line-through 位集=5");
}

#[test]
fn text_shadow_single_with_color() {
    let mut e = engine_deco(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; text-shadow: 2px 3px red; }",
        HELLO,
    );
    let op = text_op(&mut e);
    let style_engine::paint::PaintOp::Text { shadows, .. } = op else {
        panic!("not Text");
    };
    assert_eq!(shadows.len(), 1, "单影");
    let s = &shadows[0];
    assert!((s.dx - 2.0).abs() < 0.01 && (s.dy - 3.0).abs() < 0.01);
    assert!((s.blur - 0.0).abs() < 0.01, "缺省 blur=0");
    let [r, g, b, _a] = s.color.components;
    assert!(r > 0.9 && g < 0.1 && b < 0.1, "red：{r} {g} {b}");
}

#[test]
fn text_shadow_multi_with_blur() {
    let mut e = engine_deco(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; text-shadow: 1px 1px 2px blue, 4px 4px; }",
        HELLO,
    );
    let op = text_op(&mut e);
    let style_engine::paint::PaintOp::Text { shadows, .. } = op else {
        panic!("not Text");
    };
    assert_eq!(shadows.len(), 2, "逗号多影");
    assert!((shadows[0].dx - 1.0).abs() < 0.01 && (shadows[0].dy - 1.0).abs() < 0.01);
    assert!((shadows[0].blur - 2.0).abs() < 0.01, "blur 2px 解析");
    assert!((shadows[1].dx - 4.0).abs() < 0.01 && (shadows[1].dy - 4.0).abs() < 0.01);
    assert!((shadows[1].blur - 0.0).abs() < 0.01, "第二影缺省 blur=0");
    let [r, g, b, _a] = shadows[0].color.components;
    assert!(b > 0.9 && r < 0.1, "blue：{r} {g} {b}");
}

#[test]
fn text_shadow_none_is_empty() {
    let mut e = engine_deco(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; text-shadow: none; }",
        HELLO,
    );
    let op = text_op(&mut e);
    let style_engine::paint::PaintOp::Text { shadows, .. } = op else {
        panic!("not Text");
    };
    assert!(shadows.is_empty(), "none → 空影表");
}
