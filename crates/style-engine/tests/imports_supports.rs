//! B2 多样式表 + @import + @supports 锁测试。
//!
//! 契约：级联序 = 主表 + 附加表登记序（后表胜平手）；@import 附着期按
//! order 与规则流交错拼接（导入规则视同写在导入点）；layer(name) 前缀/
//! 匿名层固化；media 指令查询与规则查询 AND 合取；@supports 条件解析期
//! 求值（能力 = 属性/选择器文法）；嵌套 @media 为合取 AND（非内层覆盖）。

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

/// 结构：根(#root,id=1) → 目标(#t,id=2)；返回引擎。
fn engine_with(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    let root = StyleNode {
        id: Some("root".to_string()),
        ..StyleNode::default()
    };
    engine.insert(None, 1, root).unwrap();
    let t = StyleNode {
        id: Some("t".to_string()),
        ..StyleNode::default()
    };
    engine.insert(Some(1), 2, t).unwrap();
    engine
}

fn color_of(engine: &mut StyleEngine<u64>) -> [f32; 4] {
    let f = engine.frame((800.0, 600.0), 1.0, 0.0);
    assert!(!f.boxes.is_empty());
    let cs = engine.computed_style(2).unwrap();
    match cs.value(style_engine::css::property::PropertyId::Color) {
        Some(style_engine::css::property::DeclValue::Color(
            style_engine::css::value::ColorValue::Absolute(c),
        )) => c.components,
        other => panic!("no color: {other:?}"),
    }
}

fn assert_rgb(c: [f32; 4], r: f32, g: f32, b: f32) {
    assert!(
        (c[0] - r).abs() < 0.01 && (c[1] - g).abs() < 0.01 && (c[2] - b).abs() < 0.01,
        "expect rgb({r},{g},{b}), got {c:?}"
    );
}

// ---------- @import 拼接序 ----------

#[test]
fn import_before_rules_loses_to_later_rules() {
    // 导入规则视同写在导入点：a.css 在主规则之前 → 主表后写的 red 胜
    let mut e = engine_with("@import \"a.css\"; #t { color: red }");
    e.set_import_source("a.css", "#t { color: lime }");
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
}

#[test]
fn import_after_rules_wins_by_order() {
    // 导入点在主规则之后 → 导入规则源序更晚 → lime 胜
    let mut e = engine_with("#t { color: red } @import \"a.css\";");
    e.set_import_source("a.css", "#t { color: lime }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn import_chain_splices_recursively() {
    // a.css 自身 @import b.css——递归拼接（b 规则经链路到达，无竞争者）
    let mut e = engine_with("@import \"a.css\";");
    e.set_import_source("a.css", "@import \"b.css\";");
    e.set_import_source("b.css", "#t { color: lime }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn import_cycle_guard_terminates() {
    // 循环导入：a→b→a——守卫截断不 panic，b 规则产出一次
    let mut e = engine_with("@import \"a.css\";");
    e.set_import_source("a.css", "@import \"b.css\";");
    e.set_import_source("b.css", "@import \"a.css\"; #t { color: lime }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn import_unresolved_warns_and_skips() {
    // 未解析导入：Skipped 告警 + 无产出
    let mut e: StyleEngine<u64> = StyleEngine::new();
    let report = e.set_stylesheet("@import \"missing.css\"; #t { color: red }");
    assert!(
        !report.is_clean(),
        "unresolved import should warn: {:?}",
        report
    );
    let root = StyleNode {
        id: Some("root".to_string()),
        ..StyleNode::default()
    };
    e.insert(None, 1, root).unwrap();
    let t = StyleNode {
        id: Some("t".to_string()),
        ..StyleNode::default()
    };
    e.insert(Some(1), 2, t).unwrap();
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
}

// ---------- @import 层子句 ----------

#[test]
fn import_layer_prefix_places_rules_in_layer() {
    // layer(th) 前缀：子表规则入 th 层（子层胜父直含）→ lime 胜主表 th 直含 blue
    let mut e = engine_with("@import \"themed.css\" layer(th); @layer th { #t { color: blue } }");
    e.set_import_source("themed.css", "@layer inner { #t { color: lime } }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn import_anonymous_layer_loses_to_unlayered() {
    // layer; 匿名层：未分层主表规则（normal 轴）胜分层导入规则
    let mut e = engine_with("@import \"anon.css\" layer; #t { color: red }");
    e.set_import_source("anon.css", "#t { color: lime }");
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
}

// ---------- @import media/supports 子句 ----------

#[test]
fn import_media_applies_and_filters() {
    // media 子句命中（视口 800）：导入点在主规则后 → 导入规则源序更晚 → lime
    let mut e = engine_with("#t { color: red } @import \"m.css\" screen and (min-width: 100px);");
    e.set_import_source("m.css", "#t { color: lime }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);

    // media 子句不命中（9000px）→ 指令整体不生效 → red
    let mut e2 = engine_with("#t { color: red } @import \"m.css\" screen and (min-width: 9000px);");
    e2.set_import_source("m.css", "#t { color: lime }");
    assert_rgb(color_of(&mut e2), 1.0, 0.0, 0.0);
}

#[test]
fn import_supports_clause_gates() {
    // supports(color: red) 能力真 → 生效（导入点在后 → lime 胜）
    let mut e = engine_with("#t { color: red } @import \"s.css\" supports(color: red);");
    e.set_import_source("s.css", "#t { color: lime }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);

    // supports(显示: 未知值) 能力假 → 指令静默失效（无告警）→ red
    let mut e2 =
        engine_with("#t { color: red } @import \"s.css\" supports(display: bogusvaluezz);");
    e2.set_import_source("s.css", "#t { color: lime }");
    assert_rgb(color_of(&mut e2), 1.0, 0.0, 0.0);
}

// ---------- @supports 块 ----------

#[test]
fn supports_true_block_applies() {
    let mut e = engine_with("@supports (color: red) { #t { color: lime } }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn supports_false_block_dropped() {
    // 未知值 = 不支持 → 整块丢弃 → 默认黑
    let mut e = engine_with("@supports (display: totallybogus) { #t { color: blue } }");
    assert_rgb(color_of(&mut e), 0.0, 0.0, 0.0);
}

#[test]
fn supports_not_and_or_forms() {
    // not 反转：display: bogusvaluezz 不支持 → not → 真 → 生效
    let mut e = engine_with("@supports not (display: bogusvaluezz) { #t { color: lime } }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);

    // and 链全真
    let mut e2 = engine_with("@supports (color: red) and (width: 1px) { #t { color: lime } }");
    assert_rgb(color_of(&mut e2), 0.0, 1.0, 0.0);

    // or 链短路真
    let mut e3 = engine_with("@supports (color: bogusv1) or (color: red) { #t { color: lime } }");
    assert_rgb(color_of(&mut e3), 0.0, 1.0, 0.0);
}

#[test]
fn supports_selector_feature() {
    // selector() 特性：选择器文法可解析 = 支持
    let mut e = engine_with("@supports selector(div > .a) { #t { color: lime } }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn supports_mixed_operators_invalid() {
    // and/or 同级混用 = 语法无效 → Dropped 告警 + 规则丢弃
    let mut e: StyleEngine<u64> = StyleEngine::new();
    let report = e.set_stylesheet(
        "@supports (color: red) and (width: 1px) or (x: bogusv) { #t { color: lime } }",
    );
    assert!(!report.is_clean(), "mixed and/or should warn");
    let root = StyleNode {
        id: Some("root".to_string()),
        ..StyleNode::default()
    };
    e.insert(None, 1, root).unwrap();
    let t = StyleNode {
        id: Some("t".to_string()),
        ..StyleNode::default()
    };
    e.insert(Some(1), 2, t).unwrap();
    assert_rgb(color_of(&mut e), 0.0, 0.0, 0.0);
}

// ---------- 多表与层树文档化 ----------

#[test]
fn add_stylesheet_wins_ties_by_registration() {
    let mut e = engine_with("#t { color: red }");
    e.add_stylesheet("#t { color: blue }");
    assert_rgb(color_of(&mut e), 0.0, 0.0, 1.0);
}

#[test]
fn remove_stylesheet_restores() {
    let mut e = engine_with("#t { color: red }");
    let h = e.add_stylesheet("#t { color: blue }");
    assert_rgb(color_of(&mut e), 0.0, 0.0, 1.0);
    assert!(e.remove_stylesheet(h));
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
    assert!(!e.remove_stylesheet(h));
}

#[test]
fn extra_sheet_unlayered_beats_primary_layer() {
    // 未分层附加表规则（normal 轴）胜主表 @layer 规则——文档全局层树
    let mut e = engine_with("@layer a { #t { color: red } }");
    e.add_stylesheet("#t { color: blue }");
    assert_rgb(color_of(&mut e), 0.0, 0.0, 1.0);
}

#[test]
fn same_doc_layer_across_sheets_order_decides() {
    // 两表同层 a：文档层树共享序数；后表规则源序更晚 → lime 胜
    let mut e = engine_with("@layer a { #t { color: red } }");
    e.add_stylesheet("@layer a { #t { color: lime } }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn extra_sheet_keyframes_visible_to_animations() {
    // 附加表 @keyframes 对动画采样可见（跨表查找）
    let mut e = engine_with("#t { color: red; animation-name: fade; animation-duration: 0.5s }");
    e.add_stylesheet("@keyframes fade { from { opacity: 0 } to { opacity: 1 } }");
    let f = e.frame((800.0, 600.0), 1.0, 10.0 / 500.0);
    assert!(!f.boxes.is_empty());
    let cs = e.computed_style(2).unwrap();
    match cs.value(style_engine::css::property::PropertyId::Opacity) {
        Some(style_engine::css::property::DeclValue::Number(n)) => {
            assert!(*n > 0.0 && *n < 1.0, "opacity 应处于动画中段，实际 {n}");
        }
        other => panic!("no opacity: {other:?}"),
    }
}

// ---------- 嵌套 @media 合取（and_media 修正） ----------

#[test]
fn nested_media_conjoins_not_overwrites() {
    // 外假内真：旧"内层覆盖"会错误生效；合取 AND → 不生效 → 默认黑
    let mut e = engine_with("@media (min-width: 9000px) { @media screen { #t { color: lime } } }");
    assert_rgb(color_of(&mut e), 0.0, 0.0, 0.0);

    // 外真内假：同样不生效
    let mut e2 = engine_with("@media screen { @media (min-width: 9000px) { #t { color: lime } } }");
    assert_rgb(color_of(&mut e2), 0.0, 0.0, 0.0);

    // 双真：生效
    let mut e3 = engine_with("@media screen { @media (min-width: 700px) { #t { color: lime } } }");
    assert_rgb(color_of(&mut e3), 0.0, 1.0, 0.0);
}
