//! C1 伪元素 ::before/::after + content 锁测试（ADR-0015）。
//!
//! 语义：伪节点 = 引擎实体化的真实树子节点（::before 首子 / ::after 末子，
//! 裸 StyleNode 无身份）；选择器匹配经 selectors originating_element 回
//! origin 左复合；content none/normal（宿主节点恒 Normal）→ map_style
//! Display::None 无盒；content <string> → sync_pseudo_text 供文本测量
//!（T5c-2 同帧布局）。几何断言 = 度量尺（对齐 Chrome 盒模型行为）。

#![cfg(feature = "text")]

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

const FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../style-engine-demo/assets/fonts/DejaVuSans.ttf"
));

/// 双节点骨架：#root(1) → #t(2)。字体度量基准：伪节点经继承取得
/// font-family（未注册泛族测量 0，FEATURES.md 契约）。
fn engine_pseudo(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.add_font(FONT.to_vec());
    engine.set_stylesheet(sheet);
    let root = StyleNode {
        id: Some("root".to_string()),
        ..StyleNode::default()
    };
    engine.insert(None, 1, root).unwrap();
    let leaf = StyleNode {
        id: Some("t".to_string()),
        ..StyleNode::default()
    };
    engine.insert(Some(1), 2, leaf).unwrap();
    engine
}

/// 节点盒 (x, y, width, height)。
fn box_of(e: &mut StyleEngine<u64>, key: u64) -> (f32, f32, f32, f32) {
    let f = e.frame((800.0, 600.0), 1.0, 0.0);
    let b = f.boxes.iter().find(|b| b.key == key).unwrap();
    (b.x, b.y, b.width, b.height)
}

/// 节点 color（Absolute RGB 0..1）。
fn color_of(e: &mut StyleEngine<u64>, key: u64) -> (f32, f32, f32) {
    use style_engine::css::property::PropertyId;
    use style_engine::css::value::ColorValue;
    let f = e.frame((800.0, 600.0), 1.0, 0.0);
    let _ = f;
    let cs = e.computed_style(key).unwrap();
    match cs.value(PropertyId::Color) {
        Some(style_engine::css::property::DeclValue::Color(ColorValue::Absolute(c))) => {
            let cc = c.components;
            (cc[0], cc[1], cc[2])
        }
        _ => panic!("期望 color 绝对值"),
    }
}

#[test]
fn before_content_creates_box() {
    let mut e0 = engine_pseudo("#t { font-size: 16px; font-family: \"DejaVu Sans\"; }");
    let h0 = box_of(&mut e0, 2).3;
    let mut e1 = engine_pseudo(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; } #t::before { content: \"Hi\"; }",
    );
    let h1 = box_of(&mut e1, 2).3;
    assert!(
        h0 == 0.0 && h1 > 0.0,
        "content 串应生成文本盒（h0={h0} h1={h1}）"
    );
}

#[test]
fn after_content_appends_box() {
    let mut e = engine_pseudo(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; } #t::after { content: \"Yo\"; }",
    );
    let h = box_of(&mut e, 2).3;
    assert!(h > 0.0, "::after content 应生成文本盒（h={h}）");
}

#[test]
fn content_none_no_box() {
    let mut e = engine_pseudo(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; } #t::before { content: \"Hi\"; content: none; }",
    );
    let h = box_of(&mut e, 2).3;
    assert_eq!(h, 0.0, "content:none 覆盖后应无盒（h={h}）");
}

#[test]
fn content_normal_no_box() {
    // 无 content 声明 = normal 初始 → 无盒（字体声明不应凭空造盒）
    let mut e = engine_pseudo(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; } #t::before { font-size: 24px; }",
    );
    let h = box_of(&mut e, 2).3;
    assert_eq!(h, 0.0, "content:normal 应无盒（h={h}）");
}

#[test]
fn before_pushes_host_child_down() {
    let mut e = engine_pseudo(
        "#root { font-family: \"DejaVu Sans\"; } #root::before { content: \"P\"; font-size: 20px; }",
    );
    let y = box_of(&mut e, 2).1;
    assert!(y > 0.0, "::before 有盒应下推宿主子（y={y}）");
    let mut e2 = engine_pseudo(
        "#root { font-family: \"DejaVu Sans\"; } #root::before { content: none; font-size: 20px; }",
    );
    let y2 = box_of(&mut e2, 2).1;
    assert_eq!(y2, 0.0, "content:none 伪节点无盒不下推（y={y2}）");
}

#[test]
fn single_colon_legacy_form() {
    // CSS2 单冒号形 —— selectors is_css2_pseudo_element 路由
    let mut e = engine_pseudo(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; } #t:before { content: \"Hi\"; }",
    );
    let h = box_of(&mut e, 2).3;
    assert!(h > 0.0, "单冒号 :before 应等价解析（h={h}）");
}

#[test]
fn first_child_excludes_pseudo() {
    // 结构伪类只看宿主子：若伪节点被计入 first-child，#t 不中 :first-child
    // → 蓝色（规则序让 :first-child 胜 specificity (0,1,1) > (0,1,0)）
    let mut e = engine_pseudo(
        "#root { font-family: \"DejaVu Sans\"; } #root > :first-child { color: red; } #t { color: blue; } #root::before { content: \"P\"; font-size: 16px; }",
    );
    let c = color_of(&mut e, 2);
    assert_rgb(c, (1.0, 0.0, 0.0), "first-child 应仍匹配宿主首子 #t");
}

#[test]
fn empty_not_affected_by_pseudo() {
    // :empty 只看宿主子与文本；伪节点不影响判定
    let mut e = engine_pseudo(
        "#t:empty { color: red; } #t { color: blue; } #t::before { content: \"x\"; font-size: 16px; font-family: \"DejaVu Sans\"; }",
    );
    let c = color_of(&mut e, 2);
    assert_rgb(c, (1.0, 0.0, 0.0), ":empty 应不受伪节点实体化影响");
}

#[test]
fn origin_pseudo_class_gates_content() {
    // 伪类挂在 origin 复合上：未 hover 无盒，hover 后生成
    let mut e = engine_pseudo(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; } #t:hover::before { content: \"H\"; font-size: 16px; }",
    );
    let h0 = box_of(&mut e, 2).3;
    assert_eq!(h0, 0.0, "未 hover 不应生成盒（h={h0}）");
    e.set_state(2, style_engine::tree::NodeState::HOVER)
        .unwrap();
    let h1 = box_of(&mut e, 2).3;
    assert!(h1 > 0.0, "hover 后应生成盒（h={h1}）");
}

#[test]
fn font_inherits_from_origin() {
    let mut e16 = engine_pseudo(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; } #t::before { content: \"x\"; }",
    );
    let h16 = box_of(&mut e16, 2).3;
    let mut e32 = engine_pseudo(
        "#t { font-size: 32px; font-family: \"DejaVu Sans\"; } #t::before { content: \"x\"; }",
    );
    let h32 = box_of(&mut e32, 2).3;
    assert!(
        h32 > h16,
        "伪节点应继承 origin 字号（16px→{h16} 32px→{h32}）"
    );
}

#[test]
fn later_rule_overrides_content() {
    // 收窄容器（100px）使长串折行——伪盒 auto 宽 stretch 容器，800px 下
    // 短句不折行则两者同高（度量尺：块级盒宽度解析）。
    let mut a = engine_pseudo(
        "#root { width: 100px; } #t { font-size: 16px; font-family: \"DejaVu Sans\"; } #t::before { content: \"wrap wrap wrap wrap\"; }",
    );
    let ha = box_of(&mut a, 2).3;
    let mut b = engine_pseudo(
        "#root { width: 100px; } #t { font-size: 16px; font-family: \"DejaVu Sans\"; } #t::before { content: \"wrap wrap wrap wrap\"; } #t::before { content: \"x\"; }",
    );
    let hb = box_of(&mut b, 2).3;
    assert!(
        hb < ha,
        "后规则 content 应覆盖（短串 hb={hb} < 长串 ha={ha}）"
    );
}

#[test]
fn sheet_flip_clears_pseudos() {
    let mut e = engine_pseudo(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; } #t::before { content: \"Hi\"; }",
    );
    assert!(box_of(&mut e, 2).3 > 0.0);
    e.set_stylesheet("#t { font-size: 16px; font-family: \"DejaVu Sans\"; }");
    let h = box_of(&mut e, 2).3;
    assert_eq!(h, 0.0, "换表后无伪规则应清除全部伪节点（h={h}）");
}

#[test]
fn multi_sheet_rule_applies() {
    let mut e = engine_pseudo("#t { font-size: 16px; font-family: \"DejaVu Sans\"; }");
    e.add_stylesheet("#t::after { content: \"S\"; font-size: 16px; }");
    let h = box_of(&mut e, 2).3;
    assert!(h > 0.0, "附加表伪规则应生效（h={h}）");
}

#[test]
fn class_selector_origin() {
    let mut e = engine_pseudo(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; } .card::before { content: \"C\"; font-size: 16px; }",
    );
    // 无 class：无盒
    let h0 = box_of(&mut e, 2).3;
    assert_eq!(h0, 0.0);
    // 挂 class 后：生成
    e.set_classes(2, &["card".to_string()]).unwrap();
    let h1 = box_of(&mut e, 2).3;
    assert!(h1 > 0.0, "class 命中 origin 后应生成盒（h={h1}）");
}

/// RGB 断言（容差 0.01，克隆 imports_supports.rs 惯例）。
fn assert_rgb(got: (f32, f32, f32), want: (f32, f32, f32), what: &str) {
    assert!(
        (got.0 - want.0).abs() < 0.01
            && (got.1 - want.1).abs() < 0.01
            && (got.2 - want.2).abs() < 0.01,
        "{what}：期望 {want:?} 实得 {got:?}"
    );
}

/// 三子节点骨架：#root(1) → #t(2) → li(10/11/12)。
fn engine_list(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.add_font(FONT.to_vec());
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
    for k in [10u64, 11, 12] {
        let li = StyleNode {
            name: Some("li".into()),
            ..StyleNode::default()
        };
        engine.insert(Some(2), k, li).unwrap();
    }
    engine
}

/// 本帧全部 Text op 的文本（树序）。
fn text_ops(e: &mut StyleEngine<u64>) -> Vec<String> {
    let f = e.frame((800.0, 600.0), 1.0, 0.0);
    f.paint
        .ops
        .iter()
        .filter_map(|op| match op {
            style_engine::paint::PaintOp::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn content_counter_increments_down_tree() {
    // P5（ADR-0036 D2）：树序计数——三 li 的 ::before 计数值 1/2/3。
    let mut e = engine_list(
        "#root { font-family: \"DejaVu Sans\"; } \
         li { counter-increment: x; } \
         li::before { content: counter(x); font-size: 16px; }",
    );
    let texts = text_ops(&mut e);
    assert!(
        texts == vec!["1".to_string(), "2".to_string(), "3".to_string()],
        "计数值应树序递增 1/2/3，实得 {texts:?}"
    );
}

#[test]
fn content_counter_reset_scopes_per_node() {
    // reset 帧遮蔽：两个并列子树各自 reset+increment → 计数值都从 1 起算
    //（对照：无 reset 的递增序列）。
    let mut e = engine_list(
        "#root { font-family: \"DejaVu Sans\"; } \
         li { counter-reset: x; counter-increment: x; } \
         li::before { content: counter(x); font-size: 16px; }",
    );
    let texts = text_ops(&mut e);
    assert!(
        texts == vec!["1".to_string(), "1".to_string(), "1".to_string()],
        "reset 后每节点计数都从 1 起算，实得 {texts:?}"
    );
}

#[test]
fn content_counters_joins_scope_frames() {
    // counters(x, '.')：外层 #t reset+increment=1，li 各自 reset+increment
    // → 内层帧遮蔽 → 全帧自外向内 join = "1.1"（每 li 同值）。
    let mut e = engine_list(
        "#root { font-family: \"DejaVu Sans\"; } \
         #t { counter-reset: x; counter-increment: x; } \
         li { counter-reset: x; counter-increment: x; } \
         li::before { content: counters(x, '.'); font-size: 16px; }",
    );
    let texts = text_ops(&mut e);
    assert!(
        texts == vec!["1.1".to_string(), "1.1".to_string(), "1.1".to_string()],
        "counters() join 应为 1.1/1.1/1.1，实得 {texts:?}"
    );
}

#[test]
fn content_attr_reads_host_attribute() {
    // attr(title)：伪节点读 originating element 属性；缺失 → 空串无盒。
    // attrs 由宿主构造 StyleNode 填入（引擎无运行时 attrs 变更 API）。
    let mk = |title: Option<&str>| {
        let mut e = engine_pseudo(
            "#t { font-size: 16px; font-family: \"DejaVu Sans\"; } #t::before { content: attr(title); }",
        );
        e.remove(2).unwrap();
        let mut leaf = StyleNode {
            id: Some("t".to_string()),
            ..StyleNode::default()
        };
        if let Some(v) = title {
            leaf.attrs.insert("title".to_string(), v.to_string());
        }
        e.insert(Some(1), 2, leaf).unwrap();
        e
    };
    let mut e0 = mk(None);
    let h0 = box_of(&mut e0, 2).3;
    assert_eq!(h0, 0.0, "缺失属性 → 空串无盒");
    let mut e1 = mk(Some("VT"));
    let texts = text_ops(&mut e1);
    assert!(
        texts.contains(&"VT".to_string()),
        "attr(title) 应产出宿主属性值，实得 {texts:?}"
    );
    let h1 = box_of(&mut e1, 2).3;
    assert!(h1 > 0.0, "attr 应产出文本盒（h={h1}）");
}

#[test]
fn content_quote_pairs_depth_match() {
    // open-quote/close-quote 深度配对 + quotes 自定义对；越配 close 钳 0 静默。
    let mut e = engine_pseudo(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; quotes: '«' '»'; } \
         #t::before { content: open-quote \"x\" close-quote; }",
    );
    let texts = text_ops(&mut e);
    assert!(
        texts.contains(&"«x»".to_string()),
        "open/close 应包住内容串，实得 {texts:?}"
    );
    let mut e2 = engine_pseudo(
        "#t { font-size: 16px; font-family: \"DejaVu Sans\"; quotes: '«' '»'; } \
         #t::before { content: close-quote \"x\"; }",
    );
    let texts2 = text_ops(&mut e2);
    assert!(
        texts2.contains(&"x".to_string()),
        "越配 close-quote 钳 0 静默，实得 {texts2:?}"
    );
}

#[test]
fn content_counter_style_upper_roman() {
    // E：counter(x, upper-roman) 内置样式——additive 算法 + 负号路径不
    // 触发（1..3 正域），decimal 时代锁 "1/2/3" 的升级版。
    let mut e = engine_list(
        "#root { font-family: \"DejaVu Sans\"; } \
         li { counter-increment: x; } \
         li::before { content: counter(x, upper-roman); font-size: 16px; }",
    );
    let texts = text_ops(&mut e);
    assert!(
        texts == vec!["I".to_string(), "II".to_string(), "III".to_string()],
        "upper-roman 应渲染 I/II/III，实得 {texts:?}"
    );
}

#[test]
fn content_counter_custom_registered_style() {
    // E：@counter-style 登记 + fixed 耗尽回退——1 → 首符号，2/3 出窗 →
    // fallback decimal（缺省）。
    let mut e = engine_list(
        "#root { font-family: \"DejaVu Sans\"; } \
         @counter-style thumbs { system: fixed; symbols: \"T\"; } \
         li { counter-increment: x; } \
         li::before { content: counter(x, thumbs); font-size: 16px; }",
    );
    let texts = text_ops(&mut e);
    assert!(
        texts == vec!["T".to_string(), "2".to_string(), "3".to_string()],
        "fixed 耗尽应回退 decimal，实得 {texts:?}"
    );
}

#[test]
fn content_counter_style_range_fallback_chain() {
    // E：range 域外 → fallback 样式重走完整算法（fixed 1..2 出窗 3 →
    // upper-roman III）。
    let mut e = engine_list(
        "#root { font-family: \"DejaVu Sans\"; } \
         @counter-style cap { system: fixed; symbols: \"A\" \"B\" \"C\"; range: 1 2; fallback: upper-roman; } \
         li { counter-increment: x; } \
         li::before { content: counter(x, cap); font-size: 16px; }",
    );
    let texts = text_ops(&mut e);
    assert!(
        texts == vec!["A".to_string(), "B".to_string(), "III".to_string()],
        "range 外应链至 upper-roman，实得 {texts:?}"
    );
}

#[test]
fn content_counters_style_roman_joined() {
    // E：counters() 逐帧按样式格式化后 join（外层 I、内层各 I → "I.I"）。
    let mut e = engine_list(
        "#root { font-family: \"DejaVu Sans\"; } \
         #t { counter-reset: x; counter-increment: x; } \
         li { counter-reset: x; counter-increment: x; } \
         li::before { content: counters(x, '.', upper-roman); font-size: 16px; }",
    );
    let texts = text_ops(&mut e);
    assert!(
        texts == vec!["I.I".to_string(), "I.I".to_string(), "I.I".to_string()],
        "counters(upper-roman) join 应为 I.I，实得 {texts:?}"
    );
}

#[test]
fn content_counter_registry_overrides_builtin() {
    // E：登记表整体覆盖内置样式（css-counter-styles-3 §3）——用户重定义
    // decimal 后 counter(x, decimal) 不再是数字。
    let mut e = engine_list(
        "#root { font-family: \"DejaVu Sans\"; } \
         @counter-style decimal { system: alphabetic; symbols: \"d\" \"e\"; } \
         li { counter-increment: x; } \
         li::before { content: counter(x, decimal); font-size: 16px; }",
    );
    let texts = text_ops(&mut e);
    assert!(
        texts == vec!["d".to_string(), "e".to_string(), "dd".to_string()],
        "登记 decimal 应覆盖内置（bijective d/e），实得 {texts:?}"
    );
}
