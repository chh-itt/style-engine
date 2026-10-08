//! B3 CSS Nesting + :has() + :focus 族锁测试。
//!
//! 契约：CSS Nesting = 解析期 desugar（`&` ≡ :is(父有效源)，无 & 隐式后代
//! `:is(父) <sel>`；组合器开头同式）——级联核心零改动，嵌套规则按源序
//! 拼接进规则流；嵌套条件组（@media 等）提升穿线（条件 AND 合取、选择器
//! 继续挂父链）；嵌套层裸声明 = 体尾单条隐式 `&` 规则（B 级：声明组不按
//! 规则插入点分裂）。:has() = selectors 0.40 原生解析+匹配；表级
//! has_relative_selectors → 引擎变更类失效升级全量重样式。:focus 族 =
//! set_focus 整链迁移（焦点节点 FOCUS/FOCUS_VISIBLE + 祖先链 FOCUS_WITHIN）。

use style_engine::StyleEngine;
use style_engine::css::property::{DeclValue, PropertyId};
use style_engine::css::value::ColorValue;
use style_engine::tree::{NodeState, StyleNode};

fn node(id: &str, classes: &[&str], name: Option<&str>) -> StyleNode {
    StyleNode {
        id: Some(id.to_string()),
        classes: classes.iter().map(|s| s.to_string()).collect(),
        name: name.map(|s| s.to_string()),
        ..StyleNode::default()
    }
}

/// 结构：根(#root,id=1) → 目标(#t,id=2)；返回引擎。
fn engine_with(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    engine.insert(None, 1, node("root", &[], None)).unwrap();
    engine.insert(Some(1), 2, node("t", &[], None)).unwrap();
    engine
}

/// 结构：根(#root,id=1) → 卡片(#box,id=2,class=card) → 标题(#title,id=3)。
fn engine_card(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    engine.insert(None, 1, node("root", &[], None)).unwrap();
    engine
        .insert(Some(1), 2, node("box", &["card"], None))
        .unwrap();
    engine
        .insert(Some(2), 3, node("title", &["title"], None))
        .unwrap();
    engine
}

fn color_of_at(engine: &mut StyleEngine<u64>, id: u64) -> [f32; 4] {
    let f = engine.frame((800.0, 600.0), 1.0, 0.0);
    assert!(!f.boxes.is_empty());
    let cs = engine.computed_style(id).unwrap();
    match cs.value(PropertyId::Color) {
        Some(DeclValue::Color(ColorValue::Absolute(c))) => c.components,
        other => panic!("no color: {other:?}"),
    }
}

fn color_of(engine: &mut StyleEngine<u64>) -> [f32; 4] {
    color_of_at(engine, 2)
}

fn assert_rgb(c: [f32; 4], r: f32, g: f32, b: f32) {
    assert!(
        (c[0] - r).abs() < 0.01 && (c[1] - g).abs() < 0.01 && (c[2] - b).abs() < 0.01,
        "expect rgb({r},{g},{b}), got {c:?}"
    );
}

// ---------- CSS Nesting：desugar 语义 ----------

#[test]
fn nesting_implied_descendant() {
    // .title { } 嵌套 = :is(.card) .title（隐式后代）；卡片本体红。
    let mut e = engine_card("#box { color: red; .title { color: lime } }");
    assert_rgb(color_of_at(&mut e, 2), 1.0, 0.0, 0.0);
    assert_rgb(color_of_at(&mut e, 3), 0.0, 1.0, 0.0);
}

#[test]
fn nesting_ampersand_compound() {
    // &.big = :is(#t).big（复合，无空白插入）。
    let mut e = engine_with("#t { color: red; &.big { color: lime } }");
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);

    let mut e2: StyleEngine<u64> = StyleEngine::new();
    e2.set_stylesheet("#t { color: red; &.big { color: lime } }");
    e2.insert(None, 1, node("root", &[], None)).unwrap();
    e2.insert(Some(1), 2, node("t", &["big"], None)).unwrap();
    assert_rgb(color_of(&mut e2), 0.0, 1.0, 0.0);
}

#[test]
fn nesting_ampersand_state_pseudo() {
    // &:hover = :is(#t):hover——状态伪类挂 & 链。
    let sheet = "#t { color: red; &:hover { color: lime } }";
    let mut e = engine_with(sheet);
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);

    let mut e2: StyleEngine<u64> = StyleEngine::new();
    e2.set_stylesheet(sheet);
    e2.insert(None, 1, node("root", &[], None)).unwrap();
    let mut t = node("t", &[], None);
    t.state |= NodeState::HOVER;
    e2.insert(Some(1), 2, t).unwrap();
    assert_rgb(color_of(&mut e2), 0.0, 1.0, 0.0);
}

#[test]
fn nesting_ampersand_child_combinator() {
    // > .inner = :is(#t) > .inner（组合器开头：:is(父) 后接组合器）。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet("#box { color: red; > .inner { color: lime } }");
    e.insert(None, 1, node("root", &[], None)).unwrap();
    e.insert(Some(1), 2, node("box", &[], None)).unwrap();
    e.insert(Some(2), 3, node("inner", &["inner"], None))
        .unwrap();
    assert_rgb(color_of_at(&mut e, 2), 1.0, 0.0, 0.0);
    assert_rgb(color_of_at(&mut e, 3), 0.0, 1.0, 0.0);
}

#[test]
fn nesting_deep_two_levels() {
    // 两层嵌套：内层 & = 外层 desugar 后有效源（:is(:is(#a) .b) .c）。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet("#a { color: red; .b { color: blue; .c { color: lime } } }");
    e.insert(None, 1, node("root", &[], None)).unwrap();
    e.insert(Some(1), 2, node("a", &[], None)).unwrap();
    e.insert(Some(2), 3, node("b", &["b"], None)).unwrap();
    e.insert(Some(3), 4, node("c", &["c"], None)).unwrap();
    assert_rgb(color_of_at(&mut e, 2), 1.0, 0.0, 0.0);
    assert_rgb(color_of_at(&mut e, 3), 0.0, 0.0, 1.0);
    assert_rgb(color_of_at(&mut e, 4), 0.0, 1.0, 0.0);
}

#[test]
fn nesting_specificity_is_parent_uplift() {
    // :is(父) 提升特异性：(0,2,0) :is(.card) .title > (0,1,0) .title——
    // 即便平层规则源序更晚也输。
    let mut e = engine_card("#box { .title { color: lime } } .title { color: red }");
    assert_rgb(color_of_at(&mut e, 3), 0.0, 1.0, 0.0);
}

// ---------- CSS Nesting：嵌套条件组与边界 ----------

#[test]
fn nesting_media_declarations_inside_rule() {
    // 嵌套 @media 体内裸声明 = 隐式 & 声明规则（Chrome 兼容形态）。
    let mut e = engine_with("#t { color: red; @media (min-width: 100px) { color: lime } }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn nesting_media_false_gates() {
    // 媒体不命中 → 仅基线红。
    let mut e = engine_with("#t { color: red; @media (min-width: 900px) { color: lime } }");
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
}

#[test]
fn nesting_media_nested_rule_threading() {
    // 嵌套 @media 内嵌套规则：条件提升 + 选择器穿线（&.big 挂 #t 链）。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet("#box { @media (min-width: 100px) { &.big { color: lime } } }");
    e.insert(None, 1, node("root", &[], None)).unwrap();
    e.insert(Some(1), 2, node("box", &["big"], None)).unwrap();
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn nesting_supports_inside_rule() {
    // 嵌套 @supports 同语义（条件解析期求值）。
    let mut e = engine_with("#t { color: red; @supports (color: lime) { color: lime } }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn nesting_invalid_nested_selector_dropped() {
    // 嵌套选择器文法无效 → 规则丢弃 + 告警，基线不受影响。
    let mut e = engine_with("#t { color: red; bogus$$ { color: lime } }");
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
}

#[test]
fn top_level_media_bare_declarations_dropped() {
    // 顶层 at-rule 体裸声明（nesting_parent=None）不产生隐式规则——
    // 解析期容错丢弃，后续规则不受影响。
    let mut e = engine_with("@media (min-width: 100px) { color: red } #t { color: lime }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn nesting_custom_property_in_nested_block() {
    // 嵌套层 custom property + var() 引用贯通（内层 --c: blue 胜同名）。
    let mut e = engine_card("#box { --c: red; .title { --c: blue; color: var(--c) } }");
    assert_rgb(color_of_at(&mut e, 3), 0.0, 0.0, 1.0);
}

// ---------- :has() ----------

#[test]
fn has_child_match_and_miss() {
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet("#box:has(.title) { color: lime }");
    e.insert(None, 1, node("root", &[], None)).unwrap();
    e.insert(Some(1), 2, node("box", &[], None)).unwrap();
    e.insert(Some(2), 3, node("title", &["title"], None))
        .unwrap();
    assert_rgb(color_of_at(&mut e, 2), 0.0, 1.0, 0.0);

    let mut e2: StyleEngine<u64> = StyleEngine::new();
    e2.set_stylesheet("#box:has(.title) { color: lime }");
    e2.insert(None, 1, node("root", &[], None)).unwrap();
    e2.insert(Some(1), 2, node("box", &[], None)).unwrap();
    // 无命中 → 无 Color 冠军（默认色路径不 panic）。
    assert!(
        e2.computed_style(2)
            .and_then(|cs| cs.value(PropertyId::Color))
            .is_none()
    );
}

#[test]
fn has_descendant_match() {
    // :has 括任意深度后裔（relative selector 默认后代关系）。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet("#box:has(.title) { color: lime }");
    e.insert(None, 1, node("root", &[], None)).unwrap();
    e.insert(Some(1), 2, node("box", &[], None)).unwrap();
    e.insert(Some(2), 3, node("mid", &[], None)).unwrap();
    e.insert(Some(3), 4, node("title", &["title"], None))
        .unwrap();
    assert_rgb(color_of_at(&mut e, 2), 0.0, 1.0, 0.0);
}

#[test]
fn has_next_sibling() {
    // h1:has(+ p)：相邻兄弟相对选择器。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet("h1:has(+ p) { color: lime }");
    e.insert(None, 1, node("root", &[], None)).unwrap();
    e.insert(Some(1), 2, node("h", &[], Some("h1"))).unwrap();
    e.insert(Some(1), 3, node("p", &[], Some("p"))).unwrap();
    assert_rgb(color_of_at(&mut e, 2), 0.0, 1.0, 0.0);
}

#[test]
fn has_inside_not() {
    // :not(:has(...)) 反向组合。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet("#box:not(:has(.title)) { color: lime }");
    e.insert(None, 1, node("root", &[], None)).unwrap();
    e.insert(Some(1), 2, node("box", &[], None)).unwrap();
    assert_rgb(color_of_at(&mut e, 2), 0.0, 1.0, 0.0);

    let mut e2: StyleEngine<u64> = StyleEngine::new();
    e2.set_stylesheet("#box:not(:has(.title)) { color: lime }");
    e2.insert(None, 1, node("root", &[], None)).unwrap();
    e2.insert(Some(1), 2, node("box", &[], None)).unwrap();
    e2.insert(Some(2), 3, node("title", &["title"], None))
        .unwrap();
    assert_rgb(color_of_at(&mut e2, 2), 0.0, 0.0, 0.0);
}

#[test]
fn has_invalidation_upgrades_to_full_restyle() {
    // 关键失效契约：初始无子 → 不命中；插入 .title 子后（增量失效路径）
    // any_has_rules() 升级全量重样式 → 命中。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet("#box:has(.title) { color: lime }");
    e.insert(None, 1, node("root", &[], None)).unwrap();
    e.insert(Some(1), 2, node("box", &[], None)).unwrap();
    assert_rgb(color_of_at(&mut e, 2), 0.0, 0.0, 0.0);
    e.insert(Some(2), 3, node("title", &["title"], None))
        .unwrap();
    assert_rgb(color_of_at(&mut e, 2), 0.0, 1.0, 0.0);
}

#[test]
fn has_inside_nesting() {
    // :has 与嵌套组合：.card:has(.title) { &.big { … } }。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet("#box { &:has(.title) { &.big { color: lime } } }");
    e.insert(None, 1, node("root", &[], None)).unwrap();
    e.insert(Some(1), 2, node("box", &["big"], None)).unwrap();
    e.insert(Some(2), 3, node("title", &["title"], None))
        .unwrap();
    assert_rgb(color_of_at(&mut e, 2), 0.0, 1.0, 0.0);
}

#[test]
fn has_narrowed_invalidation_unrelated_subtree() {
    // P6（ADR-0035 D2）行为锁：`.card:has(img)` 在场时对无关子树
    // （非 host 祖先链）set_declarations → 快筛否决走增量——远端生效
    // 且 :has 命中不丢（否决不漏升级的正确性面）。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet(".card:has(img) { color: red } #note { color: blue }");
    e.insert(None, 1, node("root", &[], Some("div"))).unwrap();
    e.insert(Some(1), 2, node("c2", &["card"], Some("div")))
        .unwrap();
    e.insert(Some(2), 3, node("i3", &[], Some("img"))).unwrap();
    e.insert(Some(1), 4, node("note", &[], Some("div")))
        .unwrap();
    assert_rgb(color_of_at(&mut e, 2), 1.0, 0.0, 0.0);
    assert_rgb(color_of_at(&mut e, 4), 0.0, 0.0, 1.0);
    // 无关子树内联变更（快筛否决 → 增量路径）：远端生效、:has 命中保持。
    e.set_declarations(4, "color: green").unwrap();
    assert_rgb(color_of_at(&mut e, 4), 0.0, 0.502, 0.0);
    assert_rgb(color_of_at(&mut e, 2), 1.0, 0.0, 0.0);
    // host 子树结构变更（快筛命中 → 全量）：img 摘除后 :has 失配——
    // 红不再在场（重算后槽位回落非红值或无冠军）。
    e.remove(3).unwrap();
    let f = e.frame((800.0, 600.0), 1.0, 0.0);
    assert!(!f.boxes.is_empty());
    match e
        .computed_style(2)
        .and_then(|cs| cs.value(PropertyId::Color))
    {
        None => {}
        Some(DeclValue::Color(ColorValue::Absolute(c))) => assert!(
            (c.components[0] - 1.0).abs() > 0.5,
            ":has(img) 失配后红色仍在场: {c:?}"
        ),
        other => panic!("unexpected color: {other:?}"),
    }
}

#[test]
fn has_prefix_combinator_still_hits() {
    // P6（ADR-0035 D1）：带前缀组合器的 :has 不合格 → 哨兵全量兜底，
    // 命中行为与收窄前一致。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet(".wrap > .box:has(.title) { color: lime }");
    e.insert(None, 1, node("root", &[], Some("div"))).unwrap();
    e.insert(Some(1), 2, node("w2", &["wrap"], Some("div")))
        .unwrap();
    e.insert(Some(2), 3, node("b3", &["box"], Some("div")))
        .unwrap();
    e.insert(Some(3), 4, node("t4", &["title"], Some("div")))
        .unwrap();
    assert_rgb(color_of_at(&mut e, 3), 0.0, 1.0, 0.0);
}

// ---------- :focus 族 ----------

#[test]
fn focus_matches_after_set_focus() {
    let mut e = engine_with("#t { color: red } #t:focus { color: lime }");
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
    e.set_focus(Some(2), false).unwrap();
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn focus_visible_gated_by_flag() {
    let mut e = engine_with("#t { color: red } #t:focus-visible { color: lime }");
    e.set_focus(Some(2), false).unwrap();
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
    e.set_focus(Some(2), true).unwrap();
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn focus_within_propagates_to_ancestor() {
    // t 自带显式红（color 可继承——否则 t 会继承 root 的 lime）。
    let mut e =
        engine_with("#root { color: red } #t { color: red } #root:focus-within { color: lime }");
    e.set_focus(Some(2), false).unwrap();
    assert_rgb(color_of_at(&mut e, 1), 0.0, 1.0, 0.0);
    assert_rgb(color_of_at(&mut e, 2), 1.0, 0.0, 0.0);
}

#[test]
fn focus_move_clears_old_chain() {
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet(
        "#a { color: red } #b { color: red } #a:focus { color: lime } #b:focus { color: lime }",
    );
    e.insert(None, 1, node("root", &[], None)).unwrap();
    e.insert(Some(1), 2, node("a", &[], None)).unwrap();
    e.insert(Some(1), 3, node("b", &[], None)).unwrap();
    e.set_focus(Some(2), false).unwrap();
    assert_rgb(color_of_at(&mut e, 2), 0.0, 1.0, 0.0);
    e.set_focus(Some(3), false).unwrap();
    assert_rgb(color_of_at(&mut e, 2), 1.0, 0.0, 0.0);
    assert_rgb(color_of_at(&mut e, 3), 0.0, 1.0, 0.0);
}

#[test]
fn focus_none_clears_chain() {
    let mut e = engine_with("#t { color: red } #t:focus { color: lime }");
    e.set_focus(Some(2), false).unwrap();
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
    e.set_focus(None, false).unwrap();
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
}

#[test]
fn focus_unknown_key_errors() {
    let mut e = engine_with("#t { color: red }");
    assert!(matches!(
        e.set_focus(Some(99), false),
        Err(style_engine::error::ContractError::UnknownNode)
    ));
}

#[test]
fn focus_removed_node_clears_anchor() {
    // 焦点锚点节点被移除 → 锚点清理（后续 set_focus 正常）。
    let mut e = engine_with("#root { color: red } #root:focus { color: lime }");
    e.set_focus(Some(2), false).unwrap();
    e.remove(2).unwrap();
    e.set_focus(Some(1), false).unwrap();
    assert_rgb(color_of_at(&mut e, 1), 0.0, 1.0, 0.0);
}
