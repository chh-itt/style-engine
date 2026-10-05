//! proptest fuzz 基座（实施期 Phase 0 前置⑦；Windows 无 libFuzzer →
//! 结构化生成器路线）：五个靶点的「任何输入不 panic」+ 文档化语义锁。
//!
//! 靶点（与 15 条前置⑦ 一致）：
//! 1. 属性文法（property.rs 解析面）
//! 2. 简写展开（decl.rs expand_shorthand 各臂，经引擎 roundtrip）
//! 3. var() 代换（computed.rs CustomResolver：环检测/fallback 递归）
//! 4. @container / @media 条件解析（stylesheet.rs）
//! 5. capture_tokens 序列化回环（序列化→再解析→再序列化收敛）
//!
//! 深度预算：fallback 嵌套 ≤32——substitute 无显式深度上限（环检测已有；
//! 超深线性链的栈预算论证=T2 开放项，见 IMPLEMENTATION-LOG）。
//!
//! proptest 仅 dev-dep（C4 纪律）。

use proptest::prelude::*;
use style_engine::StyleEngine;
use style_engine::css::decl::{parse_inline_declarations, token_buf_to_string};
use style_engine::css::property::{DeclValue, PropertyId};
use style_engine::css::stylesheet::parse_stylesheet;
use style_engine::css::value::LengthPercentage;
use style_engine::tree::StyleNode;

// ---------- 生成器原子 ----------

/// 属性名池：合法长手/简写 + 伪名 + custom + 垃圾。
const NAMES: &[&str] = &[
    "width",
    "margin",
    "margin-top",
    "padding",
    "color",
    "background",
    "background-color",
    "border",
    "border-radius",
    "font",
    "font-size",
    "flex",
    "gap",
    "columns",
    "column-rule",
    "overflow",
    "transform",
    "transform-origin",
    "container",
    "animation",
    "transition",
    "opacity",
    "grid-template-columns",
    "box-shadow",
    "unknown-prop",
    "colr",
    "marginn",
    "--custom",
    "-",
];

/// 值 token 原子：合法值片段与病理片段混合（含不成对括号/引号/注释）。
const ATOMS: &[&str] = &[
    "0",
    "1px",
    "50%",
    "2em",
    "1rem",
    "10vw",
    "auto",
    "none",
    "bold",
    "italic",
    "red",
    "#fff",
    "#11223344",
    "rgb(1, 2, 3)",
    "oklch(0.5 0.1 20)",
    "light-dark(#000, #fff)",
    "calc(1px + 2%)",
    "calc(100% / (2 * 3))",
    "url(a.png)",
    "url('a.png')",
    "\"str\"",
    "'s'",
    "(",
    ")",
    "{",
    "}",
    "[",
    "]",
    ":",
    ";",
    ",",
    "!",
    "important",
    "!important",
    "var(--x)",
    "var(--x, 1px)",
    "var(--x, var(--y, 2px))",
    "translate(1px, 2px)",
    "rotate(45deg)",
    "inherit",
    "initial",
    "unset",
    "revert",
    "10deg",
    "solid",
    "dashed",
    "flex",
    "column",
    "1e3",
    "-",
    "+",
    "/",
    "*",
    "中",
    "size",
    "inline-size",
    "portrait",
    "min-width",
    "hover",
    "prefers-color-scheme",
    "dark",
];

fn atoms() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(ATOMS), 0..12).prop_map(|v| v.join(" "))
}

fn decl_text() -> impl Strategy<Value = String> {
    (
        prop::sample::select(NAMES),
        atoms(),
        prop::option::of(prop::sample::select(vec![
            " !important",
            "  !IMPORTANT",
            " ! important",
        ])),
    )
        .prop_map(|(name, value, tail)| format!("{name}: {value}{}", tail.unwrap_or("")))
}

// ---------- 靶点 1：属性文法 ----------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// 任何声明文本不 panic；块内声明 id 必为已知属性（slot() 无越界面）。
    #[test]
    fn fuzz_property_grammar_never_panics(text in decl_text()) {
        let (block, _report) = parse_inline_declarations(&text);
        for d in &block.decls {
            let _ = d.id.slot();
        }
    }

    /// 已知合法对（属性 × 文法匹配值）必须解析成功且落对槽位。
    #[test]
    fn fuzz_property_grammar_valid_pairs(
        pair in prop::sample::select(vec![
            ("width", "10px", "width"),
            ("width", "auto", "width"),
            ("margin-top", "1.5em", "margin-top"),
            ("color", "#fff", "color"),
            ("color", "rgb(1, 2, 3)", "color"),
            ("opacity", "0.5", "opacity"),
            ("overflow-x", "scroll", "overflow-x"),
            ("position", "absolute", "position"),
            ("z-index", "3", "z-index"),
            ("font-weight", "bold", "font-weight"),
            ("flex-direction", "column", "flex-direction"),
            ("text-align", "center", "text-align"),
            ("white-space", "pre", "white-space"),
            ("column-count", "3", "column-count"),
            ("aspect-ratio", "16 / 9", "aspect-ratio"),
        ])
    ) {
        let (name, value, expected) = pair;
        let (block, report) = parse_inline_declarations(&format!("{name}: {value}"));
        prop_assert!(
            report.warnings.is_empty(),
            "{name}: {value} 应合法，实际 {:?}", report.warnings
        );
        let id = PropertyId::from_css_name(expected)
            .unwrap_or_else(|| panic!("池内属性名 {expected}"));
        prop_assert!(
            block.decls.iter().any(|d| d.id == id),
            "{name}: {value} 应落槽 {expected:?}"
        );
    }
}

// ---------- 靶点 2：简写展开 ----------

/// TRBL 简写：N 值展开语义锁（1=四同 2=纵/横 3=上/横/下 4=上右下左）。
#[test]
fn shorthand_trbl_expansion_semantics() {
    let cases: &[(&str, &str, [f32; 4])] = &[
        ("margin", "1px", [1.0, 1.0, 1.0, 1.0]),
        ("margin", "1px 2px", [1.0, 2.0, 1.0, 2.0]),
        ("margin", "1px 2px 3px", [1.0, 2.0, 3.0, 2.0]),
        ("margin", "1px 2px 3px 4px", [1.0, 2.0, 3.0, 4.0]),
        ("padding", "5px 10px", [5.0, 10.0, 5.0, 10.0]),
    ];
    for &(name, value, [top, right, bottom, left]) in cases {
        let mut engine: StyleEngine<u64> = StyleEngine::new();
        engine.set_stylesheet("#root { width: 100px; height: 100px; }");
        let mut root_node = StyleNode::default();
        root_node.id = Some("root".to_string());
        engine.insert(None, 1, root_node).unwrap();
        engine
            .set_declarations(1, &format!("{name}: {value}"))
            .unwrap();
        let _frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        let cs = engine.computed_style(1).expect("computed style");
        for (side, want) in [
            ("top", top),
            ("right", right),
            ("bottom", bottom),
            ("left", left),
        ] {
            let prop = format!("{name}-{side}");
            let id = PropertyId::from_css_name(&prop).unwrap();
            let got = cs.value(id);
            // margin 族存 LenAuto（margin 允许 auto），padding 族存 Len。
            let px = match got {
                Some(DeclValue::Len(LengthPercentage::Px(x))) => Some(*x),
                Some(DeclValue::LenAuto(Some(LengthPercentage::Px(x)))) => Some(*x),
                _ => None,
            };
            assert_eq!(px, Some(want), "{name}: {value} → {prop}（{got:?}）");
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// 任意简写声明经引擎 roundtrip（解析→计算值期展开→布局→绘制）不 panic。
    #[test]
    fn fuzz_shorthand_roundtrip_never_panics(text in decl_text()) {
        let mut engine: StyleEngine<u64> = StyleEngine::new();
        engine.set_stylesheet("#root { width: 200px; height: 120px; }");
        let mut root_node = StyleNode::default();
        root_node.id = Some("root".to_string());
        engine.insert(None, 1, root_node).unwrap();
        let _ = engine.set_declarations(1, &text);
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        prop_assert!(!frame.boxes.is_empty(), "根盒恒在");
        prop_assert!(engine.computed_style(1).is_some());
    }
}

// ---------- 靶点 3：var() 代换 ----------

/// 语义锁：环 → guaranteed-invalid → fallback 生效；深链 fallback 终止。
#[test]
fn var_cycle_falls_back_and_deep_chain_terminates() {
    // ① 双节点环 + fallback：--a: var(--b); --b: var(--a) → fallback。
    // ② 32 级 fallback 链：最深 --d32: 3px，var(--d1, 1px) → 3px。
    // ③ 自环：--s: var(--s) → guaranteed-invalid。
    let mut src = String::from("#root { --a: var(--b); --b: var(--a); --s: var(--s); --d32: 3px;");
    for i in (1..32).rev() {
        src.push_str(&format!(" --d{i}: var(--d{}, 1px);", i + 1));
    }
    // 后写声明块胜出：width = --d1 深链解析 = 3px；height 用自环 --s。
    src.push_str(" width: var(--d1, 1px); height: var(--s, 7px); }");

    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(&src);
    let mut root_node = StyleNode::default();
    root_node.id = Some("root".to_string());
    engine.insert(None, 1, root_node).unwrap();
    let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
    let root = frame.find(1).expect("根盒");
    assert!(
        (root.width - 3.0).abs() < 1e-4,
        "深链 fallback 应解析 3px，实际 {}",
        root.width
    );
    // 自环 --s：guaranteed-invalid → var() 带 fallback 则 fallback 胜出
    //（css-variables-1 §3.5：被引用者为 guaranteed-invalid 时用 fallback）。
    assert!(
        (root.height - 7.0).abs() < 1e-4,
        "自环应走 fallback 7px，实际 {}",
        root.height
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// 任意 custom property 图 + var() 引用：解析/代换/布局全链不 panic。
    #[test]
    fn fuzz_var_substitution_never_panics(
        // 4 个 custom property，值含跨引用（可成环/悬垂/多级 fallback）。
        customs in prop::collection::vec(
            (0u32..4, atoms(), 0u32..3).prop_map(|(i, v, d)| {
                let refs = (0..d)
                    .map(|k| format!("var(--p{}, 1px)", (i + k + 1) % 5))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("--p{i}: {v} {refs}")
            }),
            0..4,
        ),
        usage in atoms(),
    ) {
        let mut text = customs.join(" ");
        text.push_str("; width: var(--p0, 12px); height: var(--p1, 9px)");
        let _ = &usage; // usage 并入文本以增加噪声形态
        text.push_str(&format!("; background-color: {usage}"));
        let mut engine: StyleEngine<u64> = StyleEngine::new();
        engine.set_stylesheet("#root { width: 200px; height: 120px; }");
        let mut root_node = StyleNode::default();
        root_node.id = Some("root".to_string());
        engine.insert(None, 1, root_node).unwrap();
        let _ = engine.set_declarations(1, &text);
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        prop_assert!(!frame.boxes.is_empty());
        prop_assert!(engine.computed_style(1).is_some());
    }
}

// ---------- 靶点 4：@container / @media 条件 ----------

fn at_rule_text() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["", "card", "side"])
        .prop_flat_map(|name| {
            (atoms(), 0u32..3).prop_map(move |(cond, kind)| {
                let name = if name.is_empty() {
                    String::new()
                } else {
                    format!("{name} ")
                };
                match kind {
                    0 => format!("@container {name}({cond}) {{ .q {{ width: 5px }} }}"),
                    1 => format!("@media ({cond}) {{ .q {{ width: 5px }} }}"),
                    _ => format!(
                        "@container {name}(width > 10px) {{ @media ({cond}) {{ .q {{ width: 5px }} }} }}"
                    ),
                }
            })
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// 任意 at-rule 条件：解析不 panic；引擎整链（级联+容器快照收敛）不 panic。
    #[test]
    fn fuzz_at_rule_conditions_never_panics(rule in at_rule_text()) {
        let sheet = format!("#root {{ width: 100px; height: 80px; }} {rule}");
        let parsed = parse_stylesheet(&sheet);
        let mut engine: StyleEngine<u64> = StyleEngine::new();
        engine.set_stylesheet(&sheet);
        engine.insert(None, 1, StyleNode::default()).unwrap();
        for _ in 0..3 {
            let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
            prop_assert!(!frame.boxes.is_empty());
        }
        let _ = parsed;
    }
}

// ---------- 靶点 5：capture_tokens 序列化回环 ----------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// 序列化→再解析→再序列化收敛：第二轮序列化与第一轮逐字节相等
    /// （首轮允许空白归一——needs_separator_when_before 语义）。
    #[test]
    fn fuzz_capture_tokens_roundtrip_converges(value in atoms()) {
        let src1 = format!("--x: {value}");
        let (block1, _) = parse_inline_declarations(&src1);
        let Some(buf1) = block1.custom.get("--x") else {
            return Ok(());
        };
        let ser1 = token_buf_to_string(buf1);
        let src2 = format!("--y: {ser1}");
        let (block2, _) = parse_inline_declarations(&src2);
        let Some(buf2) = block2.custom.get("--y") else {
            prop_assert!(false, "序列化产物应可再解析: {ser1:?}");
            return Ok(());
        };
        let ser2 = token_buf_to_string(buf2);
        prop_assert_eq!(ser1, ser2, "序列化应收敛");
    }
}

// ---------- 靶点 6：@font-face 块文法（F3d / ADR-0026 面） ----------

/// @font-face 描述符池：合法片段 + 病理片段混合（畸形值/悬空括号/
/// 未知描述符/var() 引用）。
const FF_DESCRIPTORS: &[&str] = &[
    "font-family: 'Fz'",
    "font-family: Fz",
    "font-family: 'Fz Two'",
    "src: url(a.woff2)",
    "src: url('a.woff2') format('woff2')",
    "src: local(Arial), url(b.woff2)",
    "src: local('Segoe UI'), url('c.woff2') format('woff2'), url(d.ttf)",
    "font-weight: 400",
    "font-weight: 100 900",
    "font-weight: bold",
    "font-style: italic",
    "font-style: oblique 14deg",
    "font-stretch: 125%",
    "font-stretch: condensed",
    "font-display: swap",
    "unicode-range: U+0-FF",
    "unicode-range: U+4E00-4FFF, U+30??",
    "font-feature-settings: \"kern\" 1",
    "font-variation-settings: \"wght\" 350",
    // 病理片段
    "src: ",
    "src: )",
    "font-family: ;",
    "font-family: 12px",
    "unicode-range: U+ZZZZ",
    "unicode-range: U+",
    "font-weight: bold extra",
    "font-stretch: 1000%",
    "unknown-desc: 1",
    "src: var(--x)",
    "}",
    ");",
];

fn font_face_text() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::option::of(prop::sample::select(FF_DESCRIPTORS)), 0..8).prop_map(
        |descs| {
            let body = descs.into_iter().flatten().collect::<Vec<_>>().join("; ");
            format!("@font-face {{ {body}; }}")
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// 任意 @font-face 块：解析 + 登记 + 引擎全链不 panic。语义锁
    /// （ADR-0026）：登记表仅存有效规则——每条必有非空 family 且 ≥1 源；
    /// stretch 描述符钳制 50–200；unicode-range 端点钳制 ≤ 0x10FFFF。
    #[test]
    fn fuzz_font_face_registry_invariants(rule in font_face_text()) {
        let sheet = format!("#root {{ width: 100px; }} {rule} .q {{ font-family: Fz; }}");
        let parsed = parse_stylesheet(&sheet);
        for f in &parsed.font_faces {
            prop_assert!(
                !f.family.is_empty() && !f.sources.is_empty(),
                "无效规则泄漏登记表: {f:?}"
            );
            if let Some(s) = f.stretch {
                prop_assert!((50.0..=200.0).contains(&s), "stretch 越界: {s}");
            }
            for &(a, b) in &f.unicode_ranges {
                prop_assert!(
                    a <= 0x10FFFF && b <= 0x10FFFF,
                    "urange 端点越界: ({a:#x},{b:#x})"
                );
            }
        }
        let mut engine: StyleEngine<u64> = StyleEngine::new();
        engine.set_stylesheet(&sheet);
        engine.insert(None, 1, StyleNode::default()).unwrap();
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        prop_assert!(!frame.boxes.is_empty());
        for f in engine.font_faces() {
            prop_assert!(
                !f.family.is_empty() && !f.sources.is_empty(),
                "引擎登记表同不变量: {f:?}"
            );
        }
    }
}

// ---------- 靶点 7：绘制文法族（gradient / clip-path / border-image / background） ----------

/// 绘制面声明池：F3b/F3c/F3d 文法（合法形态）+ 病理碎片（截断括号/
/// 空实参/悬空斜杠）。
const PAINT_DECLS: &[&str] = &[
    "background: linear-gradient(45deg, red, blue)",
    "background-image: linear-gradient(to right, #fff 0%, #000 100%), url(a.png)",
    "background-image: repeating-linear-gradient(90deg, red 0 4px, blue 4px 8px)",
    "background-image: radial-gradient(circle at 30% 30%, red, blue)",
    "background-image: radial-gradient(ellipse 40% 60% at 50% 50%, red, transparent)",
    "background-image: conic-gradient(from 90deg at 50% 50%, red, blue, green)",
    "background-image: conic-gradient(red 0 90deg, blue 90deg 180deg)",
    "background: url('a.png') no-repeat center / 50% 50%, linear-gradient(red, blue)",
    "background: url(a.png) repeat-x, url(b.png) round",
    "background-position: 10px 20px, center",
    "background-size: cover, contain",
    "clip-path: circle(50% at 50% 50%)",
    "clip-path: ellipse(40% 30% at 20% 20%)",
    "clip-path: inset(5px round 4px)",
    "clip-path: polygon(0 0, 100% 0, 50% 100%)",
    "border-image: url(x.png) 30 / 10 / 5 round",
    "border-image: linear-gradient(red, blue) 20 fill / 8 stretch",
    "border-image-source: url(x.png)",
    "border-image-slice: 30 fill",
    "border-image-outset: 5px 10px",
    "border-image-repeat: round space",
    "border-image-width: 8 4px 50% auto",
    "box-shadow: 2px 2px 4px rgba(0, 0, 0, 0.5)",
    "text-shadow: 1px 1px red, 0 0 3px blue",
    "opacity: 0.5",
    "mask-image: linear-gradient(black, transparent)",
    // 病理片段
    "background-image: linear-gradient(",
    "clip-path: circle()",
    "clip-path: polygon(",
    "border-image: url(x.png) / / /",
    "background: url(",
    "conic-gradient(none)",
];

fn paint_decl_text() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(PAINT_DECLS), 0..5).prop_map(|v| v.join("; "))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// 任意绘制声明组合：解析 → 计算 → 布局 → DisplayList → 人读视图
    /// 全链不 panic；样式面已物化。
    #[test]
    fn fuzz_paint_grammar_never_panics(decls in paint_decl_text()) {
        let mut engine: StyleEngine<u64> = StyleEngine::new();
        engine.set_stylesheet("#root { width: 200px; height: 120px; overflow: hidden; }");
        let mut root_node = StyleNode::default();
        root_node.id = Some("root".to_string());
        engine.insert(None, 1, root_node).unwrap();
        let mut child = StyleNode::default();
        child.id = Some("q".to_string());
        engine.insert(Some(1), 2, child).unwrap();
        let _ = engine.set_declarations(2, &decls);
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        prop_assert!(!frame.boxes.is_empty());
        prop_assert!(engine.computed_style(2).is_some());
        let _ = style_engine::debug::display_list_dump(&frame.paint);
    }
}

// ---------- 靶点 8：hit_test 几何不变量（ADR-0023 面） ----------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// 任意小树（负 margin / 零尺寸 / overflow hidden）× 任意采样点：
    /// hit_test 命中 ⇒ 命中节点的布局盒含该点（命中域 ⊆ 边框盒——
    /// 本树无 transform / scroll 偏移，包含关系严格成立）。
    #[test]
    fn fuzz_hit_test_contained_in_border_box(
        kids in prop::collection::vec(
            (
                prop::sample::select(&[-30i32, 0, 10, 60]),
                prop::sample::select(&[-20i32, 0, 5, 40]),
                prop::sample::select(&[0u32, 0, 20, 60, 120]),
                prop::sample::select(&[0u32, 0, 15, 40, 90]),
                0u32..2,
            ),
            1..5,
        ),
        px in -40i32..440,
        py in -40i32..340,
    ) {
        let mut engine: StyleEngine<u64> = StyleEngine::new();
        engine.set_stylesheet("#root { width: 200px; height: 200px; }");
        let mut root_node = StyleNode::default();
        root_node.id = Some("root".to_string());
        engine.insert(None, 1, root_node).unwrap();
        for (i, &(ml, mt, w, h, clip)) in kids.iter().enumerate() {
            let key = i as u64 + 2;
            let mut n = StyleNode::default();
            n.id = Some(format!("k{i}"));
            engine.insert(Some(1), key, n).unwrap();
            let mut decls = format!(
                "margin-top: {mt}px; margin-left: {ml}px; width: {w}px; height: {h}px;"
            );
            if clip == 1 {
                decls.push_str(" overflow: hidden;");
            }
            engine.set_declarations(key, &decls).unwrap();
        }
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        let mut id_to_key = std::collections::HashMap::new();
        for k in 1..=kids.len() as u64 + 1 {
            if let Some(id) = engine.node_id(&k) {
                id_to_key.insert(id, k);
            }
        }
        if let Some(hit) = engine.hit_test(px as f32, py as f32) {
            let key = id_to_key.get(&hit.node_id).expect("命中节点已注册");
            let b = frame.find(*key).expect("命中节点有布局盒");
            prop_assert!(
                px as f32 >= b.x - 1e-3
                    && px as f32 <= b.x + b.width + 1e-3
                    && py as f32 >= b.y - 1e-3
                    && py as f32 <= b.y + b.height + 1e-3,
                "命中 ({px},{py}) 不在 key={key} 盒 (x={},y={},w={},h={}) 内",
                b.x,
                b.y,
                b.width,
                b.height
            );
        }
    }
}
