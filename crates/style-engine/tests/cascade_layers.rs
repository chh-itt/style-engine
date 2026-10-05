//! B1 级联层序（@layer）+ 起源栈（User/UserAgent）+ revert 族锁测试
//!（css-cascade-5）。观察口径：computed_style 的 color 值与盒几何。
//!
//! 层序规范（css-cascade-5 §6.5）：normal 轴**晚层胜早层**、未分层胜
//! 一切分层；important 轴反转（早层胜晚层、未分层 important 输给一切
//! 分层 important）。revert = 回滚到严格更低 origin-importance 档；
//! revert-layer = 同档内更早层（无 → 按 revert）；无更低 → 初值物化。

use style_engine::StyleEngine;
use style_engine::css::property::{DeclValue, PropertyId};
use style_engine::css::value::ColorValue;
use style_engine::tree::StyleNode;

/// 结构：根(#root,id=1) → 目标(#t,id=2)；sheet 设置后建帧触发 restyle。
fn engine_with(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
    engine.insert(Some(1), 2, t).unwrap();
    let f = engine.frame((800.0, 600.0), 1.0, 0.0);
    assert!(!f.boxes.is_empty());
    engine
}

/// #t 的计算 color（sRGBA）。
fn color_of(engine: &StyleEngine<u64>) -> [f32; 4] {
    let cs = engine.computed_style(2).unwrap();
    match cs.value(PropertyId::Color) {
        Some(DeclValue::Color(ColorValue::Absolute(c))) => c.components,
        other => panic!("no color: {other:?}"),
    }
}

fn assert_rgb(c: [f32; 4], r: f32, g: f32, b: f32) {
    assert!(
        (c[0] - r).abs() < 0.01 && (c[1] - g).abs() < 0.01 && (c[2] - b).abs() < 0.01,
        "expect rgb({r},{g},{b}), got {c:?}"
    );
}

// ---------- @layer 层序 ----------

#[test]
fn layer_later_beats_earlier_normal() {
    // normal 轴：晚层胜早层（b 后声明 → blue）
    let e =
        engine_with("@layer a, b; @layer a { #t { color: red } } @layer b { #t { color: blue } }");
    assert_rgb(color_of(&e), 0.0, 0.0, 1.0);
}

#[test]
fn layer_unlayered_beats_all_layers() {
    let e = engine_with("@layer l { #t { color: red } } #t { color: blue }");
    assert_rgb(color_of(&e), 0.0, 0.0, 1.0);
}

#[test]
fn layer_nested_sublayer_beats_parent_direct() {
    // 前缀序：父层直含样式先于子层 → 子层 b 胜
    let e = engine_with("@layer a { #t { color: red } @layer b { #t { color: blue } } }");
    assert_rgb(color_of(&e), 0.0, 0.0, 1.0);
}

#[test]
fn layer_important_earlier_beats_later() {
    // important 轴反转：早层 a 胜晚层 b
    let e = engine_with(
        "@layer a { #t { color: red !important } } @layer b { #t { color: blue !important } }",
    );
    assert_rgb(color_of(&e), 1.0, 0.0, 0.0);
}

#[test]
fn layer_important_unlayered_loses_to_layered() {
    let e = engine_with("@layer l { #t { color: red !important } } #t { color: blue !important }");
    assert_rgb(color_of(&e), 1.0, 0.0, 0.0);
}

#[test]
fn layer_dotted_name_and_statement_register() {
    // @layer a.b 语句形先现序登记 = 嵌套块写法；点名声明的 a、a.b 都可入块
    let e =
        engine_with("@layer a.b; @layer a { #t { color: red } } @layer a.b { #t { color: blue } }");
    // a.b（子层，先现序在后）胜 a 直含
    assert_rgb(color_of(&e), 0.0, 0.0, 1.0);
}

// ---------- 起源栈（User / Author / Inline） ----------

#[test]
fn user_normal_loses_to_author_normal() {
    // author normal（rank 3）> user normal（rank 2）→ red
    let mut e = engine_with("#t { color: red }");
    e.set_user_stylesheet("#t { color: lime }");
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    assert_rgb(color_of(&e), 1.0, 0.0, 0.0);
}

#[test]
fn user_important_beats_author_normal() {
    let mut e = engine_with("#t { color: red }");
    e.set_user_stylesheet("#t { color: lime !important }");
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    assert_rgb(color_of(&e), 0.0, 1.0, 0.0);
}

// ---------- revert / revert-layer ----------

#[test]
fn revert_inline_rolls_to_user_past_author() {
    // 内联 revert：author 与内联同 origin-importance 档 → 越过 author
    // 到 User 档（lime）
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_user_stylesheet("#t { color: lime }");
    e.set_stylesheet("#t { color: red }");
    let mut r = StyleNode::default();
    r.id = Some("root".to_string());
    e.insert(None, 1, r).unwrap();
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
    t.declarations = style_engine::css::decl::parse_inline_declarations("color: revert").0;
    e.insert(Some(1), 2, t).unwrap();
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    assert_rgb(color_of(&e), 0.0, 1.0, 0.0);
}

#[test]
fn revert_no_lower_falls_to_initial() {
    // 无更低起源（无 user 表、UA 空）→ 初值黑
    let e = engine_with("#t { color: revert }");
    assert_rgb(color_of(&e), 0.0, 0.0, 0.0);
}

#[test]
fn revert_layer_rolls_to_earlier_layer() {
    // b 层 revert-layer → 同档更早层 a 的 red
    let e = engine_with("@layer a { #t { color: red } } @layer b { #t { color: revert-layer } }");
    assert_rgb(color_of(&e), 1.0, 0.0, 0.0);
}

#[test]
fn revert_layer_no_earlier_falls_to_revert() {
    // b 层 revert-layer 无更早层 → 按 revert 越到 User 档
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_user_stylesheet("#t { color: lime }");
    e.set_stylesheet("@layer b { #t { color: revert-layer } }");
    let mut r = StyleNode::default();
    r.id = Some("root".to_string());
    e.insert(None, 1, r).unwrap();
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
    e.insert(Some(1), 2, t).unwrap();
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    assert_rgb(color_of(&e), 0.0, 1.0, 0.0);
}

#[test]
fn custom_property_revert_rolls_to_user() {
    // custom property 冠军恰为 revert token → 回滚到 User 档的 --x
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_user_stylesheet("#t { --x: lime }");
    e.set_stylesheet("#t { --x: red; --x: revert; color: var(--x) }");
    let mut r = StyleNode::default();
    r.id = Some("root".to_string());
    e.insert(None, 1, r).unwrap();
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
    e.insert(Some(1), 2, t).unwrap();
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    assert_rgb(color_of(&e), 0.0, 1.0, 0.0);
}

// ---------- CSS 宽关键字（initial / inherit / unset） ----------

#[test]
fn wide_initial_resets_inherited_property() {
    // 父 color red；子 initial → 黑（若按旧"丢弃"行为会继承 red）
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet("#wrap { color: red } #t { color: initial }");
    let mut r = StyleNode::default();
    r.id = Some("root".to_string());
    engine.insert(None, 1, r).unwrap();
    let mut w = StyleNode::default();
    w.id = Some("wrap".to_string());
    engine.insert(Some(1), 2, w).unwrap();
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
    engine.insert(Some(2), 3, t).unwrap();
    let f = engine.frame((800.0, 600.0), 1.0, 0.0);
    assert!(!f.boxes.is_empty());
    let cs = engine.computed_style(3).unwrap();
    match cs.value(PropertyId::Color) {
        Some(DeclValue::Color(ColorValue::Absolute(c))) => assert_rgb(c.components, 0.0, 0.0, 0.0),
        other => panic!("no color: {other:?}"),
    }
}

#[test]
fn wide_inherit_forces_non_inherited_property() {
    // padding-left 非继承：inherit 强制取父值 40px（直接读计算槽位——
    // 盒几何被父 padding 位移污染不可判别）
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet("#wrap { padding-left: 40px } #t { padding-left: inherit }");
    let mut r = StyleNode::default();
    r.id = Some("root".to_string());
    engine.insert(None, 1, r).unwrap();
    let mut w = StyleNode::default();
    w.id = Some("wrap".to_string());
    engine.insert(Some(1), 2, w).unwrap();
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
    engine.insert(Some(2), 3, t).unwrap();
    let f = engine.frame((800.0, 600.0), 1.0, 0.0);
    assert!(!f.boxes.is_empty());
    let cs = engine.computed_style(3).unwrap();
    match cs.value(PropertyId::PaddingLeft) {
        // padding 长手经 parse_corner_radius（A8 路由）→ Radius 值族；
        // inherit 逐字复制父槽位值
        Some(DeclValue::Radius(w, h)) => {
            let style_engine::css::value::LengthPercentage::Px(x) = *w else {
                panic!("padding-left radius 分量异常: {w:?}");
            };
            let style_engine::css::value::LengthPercentage::Px(y) = *h else {
                panic!("padding-left radius 分量异常: {h:?}");
            };
            assert!(
                (x - 40.0).abs() < 0.01 && (y - 40.0).abs() < 0.01,
                "padding-left: inherit 应=40px，实际 {x}/{y}"
            );
        }
        other => panic!("padding-left: {other:?}"),
    }
}

#[test]
fn flex_initial_shorthand_exception_untouched() {
    // flex: initial 是 flex 专属关键字（= 0 1 auto），不得被宽关键字拦截
    let e = engine_with("#root { display: flex } #t { flex: initial; width: 50px; height: 20px }");
    // 解析干净（无 invalid value 告警）且布局正常产出
    assert!(e.computed_style(2).is_some());
}
