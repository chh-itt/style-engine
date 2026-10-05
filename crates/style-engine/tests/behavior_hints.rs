//! A1 行为提示属性锁测试（cursor/user-select/pointer-events/caret-color/
//! accent-color）：宿主可读、零绘制通道（docs/BEHAVIOR-HINT-PROPS.md）。
//!
//! 锁定面：①关键字/色值文法与槽位落点 ②继承语义（cursor/pointer-events/
//! caret-color/accent-color 继承，**user-select 不继承**）③宿主读通道
//! （ComputedStyle::value）④行为提示属性不产生任何 PaintOp（零绘制契约）。

use style_engine::StyleEngine;
use style_engine::css::decl::parse_inline_declarations;
use style_engine::css::property::{
    CursorKind, DeclValue, PointerEventsKind, PropertyId, UserSelectKind,
};
use style_engine::css::value::{ColorValue, LengthPercentage};
use style_engine::tree::StyleNode;

fn parse_value(text: &str) -> (Option<DeclValue>, usize) {
    let (block, report) = parse_inline_declarations(text);
    assert!(
        report.warnings.is_empty(),
        "{text} 应合法，实际 {:?}",
        report.warnings
    );
    let n = block.decls.len();
    (
        block.decls.into_iter().next().map(|d| match d.value {
            style_engine::css::decl::DeclSource::Parsed(v) => v,
            other => panic!("{text} 应直解析，实际 {other:?}"),
        }),
        n,
    )
}

#[test]
fn behavior_hint_keywords_parse_to_slots() {
    let cases: &[(&str, PropertyId, DeclValue)] = &[
        (
            "cursor: pointer",
            PropertyId::Cursor,
            DeclValue::Cursor(CursorKind::Pointer),
        ),
        (
            "cursor: ZOOM-IN",
            PropertyId::Cursor,
            DeclValue::Cursor(CursorKind::ZoomIn),
        ),
        (
            "cursor: not-allowed",
            PropertyId::Cursor,
            DeclValue::Cursor(CursorKind::NotAllowed),
        ),
        (
            "user-select: none",
            PropertyId::UserSelect,
            DeclValue::UserSelect(UserSelectKind::None),
        ),
        (
            "user-select: all",
            PropertyId::UserSelect,
            DeclValue::UserSelect(UserSelectKind::All),
        ),
        (
            "pointer-events: none",
            PropertyId::PointerEvents,
            DeclValue::PointerEvents(PointerEventsKind::None),
        ),
        (
            "pointer-events: auto",
            PropertyId::PointerEvents,
            DeclValue::PointerEvents(PointerEventsKind::Auto),
        ),
    ];
    for &(text, id, ref want) in cases {
        let (got, n) = parse_value(text);
        assert_eq!(n, 1, "{text} 单声明");
        assert_eq!(got.as_ref(), Some(want), "{text}");
        let _ = id.slot();
    }
}

#[test]
fn behavior_hint_color_props_auto_and_color() {
    // auto → None（与初始同型）；具体色 → Some
    let (got, _) = parse_value("caret-color: auto");
    assert_eq!(got, Some(DeclValue::CaretColor(None)));
    let (got, _) = parse_value("caret-color: #0891b2");
    assert!(
        matches!(got, Some(DeclValue::CaretColor(Some(_)))),
        "{got:?}"
    );
    let (got, _) = parse_value("accent-color: rgb(8 145 178)");
    assert!(
        matches!(got, Some(DeclValue::AccentColor(Some(_)))),
        "{got:?}"
    );
    // 非法关键字拒绝（url() 光标=T2）
    let (block, report) = parse_inline_declarations("cursor: url(a.png), pointer");
    assert!(
        block.decls.is_empty() || !report.warnings.is_empty(),
        "url() 光标应拒绝"
    );
    let (block, _) = parse_inline_declarations("user-select: widget");
    assert!(block.decls.is_empty(), "未知 user-select 值应整条丢弃");
}

#[test]
fn behavior_hint_initial_values() {
    use style_engine::computed::initial_value;
    assert_eq!(
        initial_value(PropertyId::Cursor),
        DeclValue::Cursor(CursorKind::Auto)
    );
    assert_eq!(
        initial_value(PropertyId::UserSelect),
        DeclValue::UserSelect(UserSelectKind::Auto)
    );
    assert_eq!(
        initial_value(PropertyId::PointerEvents),
        DeclValue::PointerEvents(PointerEventsKind::Auto)
    );
    assert_eq!(
        initial_value(PropertyId::CaretColor),
        DeclValue::CaretColor(None)
    );
    assert_eq!(
        initial_value(PropertyId::AccentColor),
        DeclValue::AccentColor(None)
    );
}

#[test]
fn behavior_hint_inheritance_semantics() {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(
        "#root { cursor: text; user-select: none; caret-color: #0891b2; \
         pointer-events: none; accent-color: red; }",
    );
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut child = StyleNode::default();
    child.id = Some("child".to_string());
    engine.insert(Some(1), 2, child).unwrap();
    let _ = engine.frame((400.0, 300.0), 1.0, 0.0);

    let root_cs = engine.computed_style(1).unwrap();
    let child_cs = engine.computed_style(2).unwrap();

    // 继承四件：子节点未声明 → 取父级级联值
    assert_eq!(
        child_cs.value(PropertyId::Cursor),
        Some(&DeclValue::Cursor(CursorKind::Text))
    );
    assert_eq!(
        child_cs.value(PropertyId::PointerEvents),
        Some(&DeclValue::PointerEvents(PointerEventsKind::None))
    );
    assert!(matches!(
        child_cs.value(PropertyId::CaretColor),
        Some(DeclValue::CaretColor(Some(_)))
    ));
    assert!(matches!(
        child_cs.value(PropertyId::AccentColor),
        Some(DeclValue::AccentColor(Some(_)))
    ));
    // user-select 不继承：子节点槽位=初始 auto（全集物化），而非父级 none
    assert_eq!(
        child_cs.value(PropertyId::UserSelect),
        Some(&DeclValue::UserSelect(UserSelectKind::Auto))
    );
    assert_eq!(
        root_cs.value(PropertyId::UserSelect),
        Some(&DeclValue::UserSelect(UserSelectKind::None))
    );
    // 子节点显式声明胜出
    engine.set_declarations(2, "user-select: text").unwrap();
    let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
    let child_cs = engine.computed_style(2).unwrap();
    assert_eq!(
        child_cs.value(PropertyId::UserSelect),
        Some(&DeclValue::UserSelect(UserSelectKind::Text))
    );
}

#[test]
fn behavior_hint_no_paint_ops() {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet("#root { cursor: grab; user-select: none; caret-color: red; }");
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let plain = engine.frame((400.0, 300.0), 1.0, 0.0);
    engine.set_declarations(1, "cursor: grab; user-select: none; caret-color: red; accent-color: blue; pointer-events: none").unwrap();
    let painted = engine.frame((400.0, 300.0), 1.0, 0.0);
    assert_eq!(
        plain.paint.ops, painted.paint.ops,
        "行为提示属性不得产生任何绘制差异"
    );
    assert!(
        painted
            .paint
            .ops
            .iter()
            .all(|op| !matches!(op, style_engine::paint::PaintOp::Text { .. })),
        "无文本内容时不应有文本基元"
    );
    // 几何亦不因行为提示属性变化
    assert_eq!(plain.boxes, painted.boxes);
}

#[test]
fn behavior_hint_color_value_shape() {
    // caret-color 解析产物与初始同型校验（七件套：初始同型）
    let (got, _) = parse_value("caret-color: rgb(0 0 0 / 50%)");
    match got {
        Some(DeclValue::CaretColor(Some(ColorValue::Absolute(c)))) => {
            assert!((c.components[3] - 0.5).abs() < 1e-4, "alpha 50% 应解析");
        }
        other => panic!("半透明色应落 Absolute，实际 {other:?}"),
    }
    let _ = LengthPercentage::Px(0.0);
}
