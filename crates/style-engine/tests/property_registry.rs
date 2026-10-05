//! B4 @property 注册（css-properties-values-api）锁测试。
//!
//! 契约（ADR-0014）：注册三描述符（syntax/inherits/initial-value）；
//! 非 universal 缺 initial 或 initial 不匹配 → 注册无效（Dropped）；
//! 计算语义 = 继承门（inherits:false 不进继承通道）+ 语法门（终值不匹配
//! → unset → initial-value，Chrome 一致：var(--x) 解析到 initial 而非
//! 触发 fallback）+ initial 填充（声明/继承双缺 → initial-value）；
//! 注册表合并序 user → primary → extras，同名后者胜；@property 仅顶层
//! 合法（嵌套/条件组内丢弃）；universal = 不校验、声明值原样在场。

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

/// 结构：根(#root,id=1) → 目标(#t,id=2)；返回引擎。
fn engine_with(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
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

const REG_COLOR: &str =
    "@property --x { syntax: \"<color>\"; inherits: false; initial-value: red; } ";

// ---------- 注册 + initial ----------

#[test]
fn registered_initial_fills_unset() {
    // --x 未声明/未继承 → initial-value red 进 final_custom → var 直通
    let mut e = engine_with(&format!("{REG_COLOR}#t {{ color: var(--x); }}"));
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
}

#[test]
fn declared_value_passes_gate() {
    // 声明值匹配 <color> → 语法门通过 → lime
    let mut e = engine_with(&format!("{REG_COLOR}#t {{ --x: lime; color: var(--x); }}"));
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn invalid_declaration_falls_to_initial() {
    // 声明值不匹配 <color> → unset → initial red（Chrome 一致）
    let mut e = engine_with(&format!("{REG_COLOR}#t {{ --x: 42px; color: var(--x); }}"));
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
}

#[test]
fn gate_failure_does_not_trigger_fallback() {
    // 关键 Chrome 语义：门失败 → initial（var(--x, lime) 取 red 而非
    // fallback lime——fallback 仅用于缺席名）
    let mut e = engine_with(&format!(
        "{REG_COLOR}#t {{ --x: 42px; color: var(--x, lime); }}"
    ));
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
}

#[test]
fn unregistered_name_uses_fallback() {
    // 未注册名且无声明 → 缺席 → fallback 生效
    let mut e = engine_with("#t { color: var(--nope, lime); }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

// ---------- 继承门 ----------

#[test]
fn inherits_false_child_gets_initial() {
    // inherits:false → 父声明不进子代继承通道 → 子代取 initial red
    let mut e = engine_with(&format!(
        "{REG_COLOR}#root {{ --x: lime; }} #t {{ color: var(--x); }}"
    ));
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
}

#[test]
fn inherits_true_child_inherits() {
    // inherits:true → 父值 lime 进继承通道 → 子代 lime
    let mut e = engine_with(
        "@property --x { syntax: \"<color>\"; inherits: true; initial-value: red; } \
         #root { --x: lime; } #t { color: var(--x); }",
    );
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn unregistered_custom_still_inherits() {
    // 未注册 custom property 行为不变（可继承、无门）
    let mut e = engine_with("#root { --y: olive; } #t { color: var(--y); }");
    assert_rgb(color_of(&mut e), 0.5, 0.5, 0.0);
}

// ---------- 注册表合并 ----------

#[test]
fn multi_sheet_later_registration_wins() {
    // 主表 <color> initial red：--x: lime 过门 → lime
    let mut e = engine_with(&format!(
        "{REG_COLOR}#t {{ --x: lime; color: var(--x, black); }}"
    ));
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
    // 附加表同名覆盖为 <length> initial 1px：--x: lime 门败 → initial
    // "1px" → color 解析败 → IACVT → 初始黑
    let mut e2 = engine_with(&format!(
        "{REG_COLOR}#t {{ --x: lime; color: var(--x, black); }}"
    ));
    e2.add_stylesheet("@property --x { syntax: \"<length>\"; initial-value: 1px; }");
    assert_rgb(color_of(&mut e2), 0.0, 0.0, 0.0);
}

// ---------- universal ----------

#[test]
fn universal_skips_gate_value_stays_present() {
    // universal：无校验、无 initial——垃圾声明值原样在场（var 代换后
    // color 解析败 → IACVT 黑），与未注册（fallback lime）区分
    let mut e = engine_with(
        "@property --x { syntax: \"*\"; } #t { --x: any garbage; color: var(--x, lime); }",
    );
    assert_rgb(color_of(&mut e), 0.0, 0.0, 0.0);
    // universal 合法值直通
    let mut e2 =
        engine_with("@property --x { syntax: \"*\"; } #t { --x: navy; color: var(--x, black); }");
    assert_rgb(color_of(&mut e2), 0.0, 0.0, 0.5);
}

// ---------- 注册无效 → 丢弃 ----------

#[test]
fn registration_missing_initial_dropped() {
    // 非 universal 且缺 initial-value → 注册无效 → 未注册语义（fallback）
    let mut e = engine_with(
        "@property --x { syntax: \"<color>\"; inherits: false; } #t { color: var(--x, lime); }",
    );
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn registration_initial_mismatch_dropped() {
    // initial-value 与 syntax 不匹配 → 注册无效
    let mut e = engine_with(
        "@property --x { syntax: \"<color>\"; initial-value: 42px; } #t { color: var(--x, lime); }",
    );
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

// ---------- 非法语境 ----------

#[test]
fn nested_property_ignored() {
    // 样式规则体内 @property = 无效语境 → 丢弃告警 → 未注册语义
    let mut e = engine_with(
        "#root { @property --x { syntax: \"<color>\"; initial-value: red; } } \
         #t { color: var(--x, lime); }",
    );
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn media_wrapped_property_ignored() {
    // 条件组内 @property = 无效语境（spec：仅顶层合法）→ 丢弃
    let mut e = engine_with(
        "@media (min-width: 100px) { @property --x { syntax: \"<color>\"; initial-value: red; } } \
         #t { color: var(--x, lime); }",
    );
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

#[test]
fn statement_form_invalid() {
    // @property --x;（无块体）= 无效规则 → 丢弃 → fallback
    let mut e = engine_with("@property --x; #t { color: var(--x, lime); }");
    assert_rgb(color_of(&mut e), 0.0, 1.0, 0.0);
}

// ---------- 多值组合子 + 类型面 ----------

#[test]
fn plus_multi_initial_validated() {
    // <length>+：initial "1px 2px" 合法 → 注册成功 → --x 在场（initial
    // 填充）→ color 解析败 → IACVT 黑（非 fallback lime）
    let mut e = engine_with(
        "@property --x { syntax: \"<length>+\"; initial-value: 1px 2px; } \
         #t { color: var(--x, lime); }",
    );
    assert_rgb(color_of(&mut e), 0.0, 0.0, 0.0);
    // initial "1px red" 不匹配 <length>+ → 注册无效 → fallback lime
    let mut e2 = engine_with(
        "@property --x { syntax: \"<length>+\"; initial-value: 1px red; } \
         #t { color: var(--x, lime); }",
    );
    assert_rgb(color_of(&mut e2), 0.0, 1.0, 0.0);
}

#[test]
fn hash_multi_and_single_item() {
    // <color>#：单条目合法（# = ≥1 逗号分隔）→ red 直通
    let mut e = engine_with(
        "@property --x { syntax: \"<color>#\"; initial-value: blue; } \
         #t { --x: red; color: var(--x); }",
    );
    assert_rgb(color_of(&mut e), 1.0, 0.0, 0.0);
    // 空白分隔项不匹配 <color>#（需逗号）→ 门败 → initial blue
    let mut e2 = engine_with(
        "@property --x { syntax: \"<color>#\"; initial-value: blue; } \
         #t { --x: red green; color: var(--x); }",
    );
    assert_rgb(color_of(&mut e2), 0.0, 0.0, 1.0);
}

#[test]
fn custom_ident_gate() {
    // <custom-ident>：ident 过门；数字门败 → initial "foo"（color 解析
    // 败 → IACVT 黑）
    let mut e = engine_with(
        "@property --x { syntax: \"<custom-ident>\"; initial-value: foo; } \
         #t { --x: bar; color: var(--x, lime); }",
    );
    // "bar" 非法 color → IACVT 黑（--x 在场）
    assert_rgb(color_of(&mut e), 0.0, 0.0, 0.0);
    let mut e2 = engine_with(
        "@property --x { syntax: \"<custom-ident>\"; initial-value: foo; } \
         #t { --x: 42; color: var(--x, lime); }",
    );
    // 门败 → initial "foo" → 仍非法 color → IACVT 黑（非 fallback lime）
    assert_rgb(color_of(&mut e2), 0.0, 0.0, 0.0);
}
