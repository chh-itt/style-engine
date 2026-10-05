//! A8 逻辑属性（css-logical-1）+ direction/unicode-bidi 锁测试：
//! ①ltr/rtl 逻辑→物理映射（inline-start=left / rtl 翻转）②物理/逻辑
//! 级联序键定夺（同规则块按声明序 decl_index、跨规则按源序 order）
//! ③双轴简写 1-2 值形 ④border 逻辑集 6 长hand ⑤direction 继承 +
//! unicode-bidi 计算可读 ⑥逻辑槽自身不外泄（写位，物理槽承载终值）。

use style_engine::StyleEngine;
use style_engine::css::decl::parse_inline_declarations;
use style_engine::css::property::{
    BorderStyle, DeclValue, DirectionKind, PropertyId, UnicodeBidiKind,
};
use style_engine::css::value::{ColorValue, LengthPercentage};
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

/// 根(#root, id=1) + 子(#t, id=2) 双节点样板；sheet 可含两节点规则。
fn tree_with(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut node = StyleNode::default();
    node.id = Some("t".to_string());
    engine.insert(Some(1), 2, node).unwrap();
    engine
}

fn px(v: f32) -> DeclValue {
    DeclValue::LenAuto(Some(LengthPercentage::Px(v)))
}

fn len_px(v: f32) -> DeclValue {
    DeclValue::Len(LengthPercentage::Px(v))
}

// ---------- ① ltr/rtl 映射 ----------

#[test]
fn ltr_maps_inline_start_to_left() {
    let mut engine = tree_with("#t { margin-inline-start: 20px; }");
    let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
    let cs = engine.computed_style(2).unwrap();
    assert_eq!(cs.value(PropertyId::MarginLeft), Some(&px(20.0)));
    assert_eq!(cs.value(PropertyId::MarginRight), Some(&px(0.0)));
    // 逻辑槽是写位：终值由映射物理槽承载，逻辑槽不外泄映射值
    assert!(matches!(
        cs.value(PropertyId::MarginInlineStart),
        None | Some(DeclValue::LenAuto(Some(LengthPercentage::Px(0.0))))
    ));
}

#[test]
fn rtl_flips_inline_axis() {
    let mut engine = tree_with("#root { direction: rtl; } #t { margin-inline-start: 20px; }");
    let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
    let cs = engine.computed_style(2).unwrap();
    // rtl：inline-start = 右侧
    assert_eq!(cs.value(PropertyId::MarginRight), Some(&px(20.0)));
    assert_eq!(cs.value(PropertyId::MarginLeft), Some(&px(0.0)));
}

#[test]
fn rtl_maps_inset_and_border_start() {
    let mut engine = tree_with(
        "#root { direction: rtl; } #t { inset-inline-start: 8px; border-inline-start-width: 3px; }",
    );
    let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
    let cs = engine.computed_style(2).unwrap();
    assert_eq!(cs.value(PropertyId::Right), Some(&px(8.0)));
    assert_eq!(cs.value(PropertyId::Left), Some(&DeclValue::LenAuto(None)));
    assert_eq!(
        cs.value(PropertyId::BorderRightWidth),
        Some(&DeclValue::BorderWidth(Some(LengthPercentage::Px(3.0))))
    );
}

#[test]
fn block_axis_unaffected_by_rtl() {
    let mut engine = tree_with("#root { direction: rtl; } #t { margin-block-start: 6px; }");
    let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
    let cs = engine.computed_style(2).unwrap();
    assert_eq!(cs.value(PropertyId::MarginTop), Some(&px(6.0)));
}

// ---------- ② 物理/逻辑级联序键定夺 ----------

#[test]
fn same_block_declaration_order_decides() {
    // 同规则块：逻辑在后 → 逻辑胜（映射覆盖物理）
    let mut engine = tree_with("#t { margin-left: 10px; margin-inline-start: 30px; }");
    let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
    assert_eq!(
        engine
            .computed_style(2)
            .unwrap()
            .value(PropertyId::MarginLeft),
        Some(&px(30.0))
    );
    // 同规则块：物理在后 → 物理胜
    let mut engine = tree_with("#t { margin-inline-start: 30px; margin-left: 10px; }");
    let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
    assert_eq!(
        engine
            .computed_style(2)
            .unwrap()
            .value(PropertyId::MarginLeft),
        Some(&px(10.0))
    );
}

#[test]
fn cross_rule_source_order_decides() {
    // 跨规则同特异性：后规则胜（与逻辑/物理无关）
    let cases: [(&str, f32); 2] = [
        (
            "#t { margin-inline-start: 30px; } #t { margin-left: 10px; }",
            10.0,
        ),
        (
            "#t { margin-left: 10px; } #t { margin-inline-start: 30px; }",
            30.0,
        ),
    ];
    for (sheet, want) in cases {
        let mut engine = tree_with(sheet);
        let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert_eq!(
            engine
                .computed_style(2)
                .unwrap()
                .value(PropertyId::MarginLeft),
            Some(&px(want)),
            "{sheet}"
        );
    }
}

// ---------- ③④ 简写展开 ----------

#[test]
fn logical_shorthand_two_value_forms() {
    // 1 值双侧
    let decls = parsed("margin-inline: 5px");
    assert_eq!(decls.len(), 2);
    assert!(
        decls
            .iter()
            .any(|(id, v)| *id == PropertyId::MarginInlineStart && *v == px(5.0))
    );
    assert!(
        decls
            .iter()
            .any(|(id, v)| *id == PropertyId::MarginInlineEnd && *v == px(5.0))
    );
    // 2 值 start end
    let decls = parsed("padding-inline: 10px 20px");
    assert_eq!(decls.len(), 2);
    assert!(
        decls
            .iter()
            .any(|(id, v)| *id == PropertyId::PaddingInlineStart && *v == len_px(10.0))
    );
    assert!(
        decls
            .iter()
            .any(|(id, v)| *id == PropertyId::PaddingInlineEnd && *v == len_px(20.0))
    );
    // auto 混排（inset 族 LenAuto 值域）
    let decls = parsed("inset-block: auto 4px");
    assert!(
        decls
            .iter()
            .any(|(id, v)| *id == PropertyId::InsetBlockStart && *v == DeclValue::LenAuto(None))
    );
    assert!(
        decls
            .iter()
            .any(|(id, v)| *id == PropertyId::InsetBlockEnd && *v == px(4.0))
    );
    // 三值非法 → 整条丢弃（简写值表必须穷尽）
    let (block, report) = parse_inline_declarations("margin-inline: 1px 2px 3px");
    assert!(
        block.decls.is_empty() && !report.warnings.is_empty(),
        "margin-inline 三值应整条非法"
    );
}

#[test]
fn border_inline_expands_six_longhands() {
    let decls = parsed("border-inline: 2px solid red");
    assert_eq!(decls.len(), 6, "start+end × width/style/color");
    for pid in [
        PropertyId::BorderInlineStartWidth,
        PropertyId::BorderInlineEndWidth,
    ] {
        assert!(
            decls.iter().any(|(id, v)| *id == pid
                && *v == DeclValue::BorderWidth(Some(LengthPercentage::Px(2.0))))
        );
    }
    for pid in [
        PropertyId::BorderInlineStartStyle,
        PropertyId::BorderInlineEndStyle,
    ] {
        assert!(
            decls
                .iter()
                .any(|(id, v)| *id == pid && *v == DeclValue::BorderStyle(BorderStyle::Solid))
        );
    }
    for pid in [
        PropertyId::BorderInlineStartColor,
        PropertyId::BorderInlineEndColor,
    ] {
        assert!(decls.iter().any(|(id, _)| *id == pid));
    }
    // 部分分量重置初始（width medium/style none/color currentcolor）
    let decls = parsed("border-block: dashed");
    assert_eq!(decls.len(), 6);
    assert!(
        decls
            .iter()
            .any(|(id, v)| *id == PropertyId::BorderBlockStartStyle
                && *v == DeclValue::BorderStyle(BorderStyle::Dashed))
    );
    assert!(
        decls
            .iter()
            .any(|(id, v)| *id == PropertyId::BorderBlockStartWidth
                && *v == DeclValue::BorderWidth(Some(LengthPercentage::Px(3.0))))
    );
}

#[test]
fn shorthand_flows_through_cascade_per_direction() {
    // rtl 下 padding-inline 的 start 落右侧
    let mut engine = tree_with("#root { direction: rtl; } #t { padding-inline: 10px 20px; }");
    let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
    let cs = engine.computed_style(2).unwrap();
    assert_eq!(cs.value(PropertyId::PaddingRight), Some(&len_px(10.0)));
    assert_eq!(cs.value(PropertyId::PaddingLeft), Some(&len_px(20.0)));
    // border-inline 经样式表路径同样 6 长hand 生效
    let mut engine = tree_with("#t { border-inline: 2px solid red; }");
    let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
    let cs = engine.computed_style(2).unwrap();
    assert_eq!(
        cs.value(PropertyId::BorderLeftStyle),
        Some(&DeclValue::BorderStyle(BorderStyle::Solid))
    );
    assert_eq!(
        cs.value(PropertyId::BorderRightStyle),
        Some(&DeclValue::BorderStyle(BorderStyle::Solid))
    );
    assert!(matches!(
        cs.value(PropertyId::BorderLeftColor),
        Some(DeclValue::Color(ColorValue::Absolute(_)))
    ));
}

// ---------- ⑤ direction 继承 / unicode-bidi ----------

#[test]
fn direction_inherits_and_bidi_readable() {
    let mut engine = tree_with("#root { direction: rtl; } #t { unicode-bidi: isolate; }");
    let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
    let cs = engine.computed_style(2).unwrap();
    assert_eq!(cs.direction(), DirectionKind::Rtl, "direction 继承");
    assert_eq!(cs.unicode_bidi(), UnicodeBidiKind::Isolate);
    // 本例根显式 rtl（子继承断言的来源）
    let cs_root = engine.computed_style(1).unwrap();
    assert_eq!(cs_root.direction(), DirectionKind::Rtl);
    assert_eq!(cs_root.unicode_bidi(), UnicodeBidiKind::Normal);
    // 缺省：无声明根为 ltr / normal
    let mut engine = tree_with("#root { width: 10px; }");
    let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
    let cs_root = engine.computed_style(1).unwrap();
    assert_eq!(cs_root.direction(), DirectionKind::Ltr);
    assert_eq!(cs_root.unicode_bidi(), UnicodeBidiKind::Normal);
}
