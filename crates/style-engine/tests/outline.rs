//! A2 outline/outline-offset 锁测试：不占布局的装饰描边（ink overflow，
//! css-ui-4）。绘制复用 Border 基元以外扩矩形承载——描边带 =
//! [border-box+offset, border-box+offset+width]；radius 随外扩量增长。
//!
//! 锁定面：①简写 `||` 文法（任意序/部分分量重置初始；**不重置
//! outline-offset**）②长手+offset 负值 ③op 几何精确锁 ④none/零宽不发射
//! ⑤布局与滚动量程零影响 ⑥auto→Solid 近似（B 级在案）。

use style_engine::StyleEngine;
use style_engine::css::decl::parse_inline_declarations;
use style_engine::css::property::BorderStyle;
use style_engine::css::property::{DeclValue, OutlineStyle, PropertyId};
use style_engine::css::value::LengthPercentage;
use style_engine::paint::{BorderSide, PaintOp};
use style_engine::tree::StyleNode;

fn parsed(text: &str) -> Vec<(PropertyId, DeclValue)> {
    let (block, report) = parse_inline_declarations(text);
    assert!(
        report.warnings.is_empty(),
        "{text} 应合法，实际 {:?}",
        report.warnings
    );
    block
        .decls
        .into_iter()
        .map(|d| match d.value {
            style_engine::css::decl::DeclSource::Parsed(v) => (d.id, v),
            other => panic!("{text} 应直解析，实际 {other:?}"),
        })
        .collect()
}

#[test]
fn outline_shorthand_any_order_and_resets() {
    // 任意序：width||style||color 全分量
    for text in [
        "outline: solid 2px red",
        "outline: red solid 2px",
        "outline: 2px solid red",
        "outline: red 2px solid",
    ] {
        let decls = parsed(text);
        assert_eq!(decls.len(), 3, "{text} 应展开三长手");
        assert!(decls.iter().any(|(id, v)| *id == PropertyId::OutlineWidth
            && *v == DeclValue::BorderWidth(Some(LengthPercentage::Px(2.0)))));
        assert!(decls.iter().any(|(id, v)| *id == PropertyId::OutlineStyle
            && *v == DeclValue::OutlineStyle(OutlineStyle::Solid)));
        assert!(decls.iter().any(|(id, _)| *id == PropertyId::OutlineColor));
    }
    // 部分分量：未指定长手重置初始（width medium/style none/color currentcolor）
    let decls = parsed("outline: 1px");
    assert!(decls.iter().any(|(id, v)| *id == PropertyId::OutlineWidth
        && *v == DeclValue::BorderWidth(Some(LengthPercentage::Px(1.0)))));
    assert!(decls.iter().any(|(id, v)| *id == PropertyId::OutlineStyle
        && *v == DeclValue::OutlineStyle(OutlineStyle::None)));
    // 简写不含 offset：offset 长手独立解析、不受简写影响
    let decls = parsed("outline: solid");
    assert!(decls.iter().all(|(id, _)| *id != PropertyId::OutlineOffset));
}

#[test]
fn outline_longhands_and_negative_offset() {
    let decls = parsed("outline-width: thin; outline-style: dotted; outline-offset: -2px");
    assert!(decls.iter().any(|(id, v)| *id == PropertyId::OutlineWidth
        && *v == DeclValue::BorderWidth(Some(LengthPercentage::Px(1.0)))));
    assert!(decls.iter().any(|(id, v)| *id == PropertyId::OutlineStyle
        && *v == DeclValue::OutlineStyle(OutlineStyle::Dotted)));
    assert!(decls.iter().any(|(id, v)| *id == PropertyId::OutlineOffset
        && *v == DeclValue::Len(LengthPercentage::Px(-2.0))));
    // 非法线型整条丢弃
    let (block, _) = parse_inline_declarations("outline-style: wavy");
    assert!(block.decls.is_empty());
}

/// 根 100×100，outline: solid 2px red，offset 4px → 恰一个 Border op：
/// x=-6/y=-6/112×112（d=offset+width=6），四边 width 2 Solid，radius 全 6。
#[test]
fn outline_op_geometry_expanded_rect() {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(
        "#root { width: 100px; height: 100px; background: #fff; \
         outline: solid 2px red; outline-offset: 4px; }",
    );
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
    let borders: Vec<&PaintOp> = frame
        .paint
        .ops
        .iter()
        .filter(|op| matches!(op, PaintOp::Border { .. }))
        .collect();
    assert_eq!(
        borders.len(),
        1,
        "恰一个 outline Border op，实际 {borders:?}"
    );
    let Some(PaintOp::Border {
        x,
        y,
        width,
        height,
        radius,
        sides,
    }) = borders.first().copied()
    else {
        unreachable!()
    };
    assert_eq!((*x, *y, *width, *height), (-6.0, -6.0, 112.0, 112.0));
    assert!(
        radius.iter().all(|r| (*r - 6.0).abs() < 1e-4),
        "radius 全 6（+d），实际 {radius:?}"
    );
    let want = BorderSide {
        width: 2.0,
        style: BorderStyle::Solid,
        color: peniko_color_red(),
    };
    for side in sides {
        assert_eq!(*side, want);
    }
}

fn peniko_color_red() -> style_engine::AlphaColor<style_engine::Srgb> {
    style_engine::AlphaColor::new([1.0, 0.0, 0.0, 1.0])
}

#[test]
fn outline_none_or_zero_width_emits_nothing() {
    for extra in ["outline: none", "outline: solid 0px", "outline-width: 0"] {
        let mut engine: StyleEngine<u64> = StyleEngine::new();
        engine.set_stylesheet(&format!(
            "#root {{ width: 100px; height: 100px; background: #fff; {extra} }}"
        ));
        let mut root = StyleNode::default();
        root.id = Some("root".to_string());
        engine.insert(None, 1, root).unwrap();
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert!(
            !frame
                .paint
                .ops
                .iter()
                .any(|op| matches!(op, PaintOp::Border { .. })),
            "{extra} 不应产生描边 op"
        );
    }
}

#[test]
fn outline_no_layout_or_scroll_range_impact() {
    let base_sheet = "#root { width: 100px; height: 100px; background: #fff; }";
    let with_sheet = "#root { width: 100px; height: 100px; background: #fff; outline: solid 8px red; outline-offset: 6px; }";
    let frame_for = |sheet: &str| {
        let mut engine: StyleEngine<u64> = StyleEngine::new();
        engine.set_stylesheet(sheet);
        let mut root = StyleNode::default();
        root.id = Some("root".to_string());
        engine.insert(None, 1, root).unwrap();
        engine.frame((400.0, 300.0), 1.0, 0.0)
    };
    let plain = frame_for(base_sheet);
    let outlined = frame_for(with_sheet);
    // 不占布局：盒逐位等；不入滚动量程（ink overflow）
    assert_eq!(plain.boxes, outlined.boxes);
    assert_eq!(plain.scrollable, outlined.scrollable);
    assert!(
        outlined.paint.ops.len() > plain.paint.ops.len(),
        "描边 op 在场"
    );
}

#[test]
fn outline_auto_draws_solid_approximation() {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet("#root { width: 100px; height: 100px; outline: auto; }");
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
    let outline = frame.paint.ops.iter().find_map(|op| match op {
        PaintOp::Border { sides, .. } => Some(sides[0].style),
        _ => None,
    });
    assert_eq!(
        outline,
        Some(BorderStyle::Solid),
        "auto 按 Solid 近似（B 级在案）"
    );
}
