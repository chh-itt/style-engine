//! P9-3 列表闭环锁测试（css-lists-3）。
//!
//! 度量尺 = marker 文本（DisplayList Text op 序与颜色）+ 布局几何。
//! 语义：marker 伪节点由引擎机器合成（宿主首子、::before 之前）；
//! list-item 隐式 list-item 计数器（§4.6）；非 list-item 抑制成盒
//!（§3.1）；list-style-type none/string/counter-style 名三形态。

#![cfg(feature = "text")]

use style_engine::StyleEngine;
use style_engine::builtins::DEFAULT_UA_SHEET;
use style_engine::tree::StyleNode;

const FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../style-engine-demo/assets/fonts/DejaVuSans.ttf"
));

/// 带 UA 表的引擎骨架：#root(1) 挂任意树。字体族显式声明（缺省
/// SansSerif 泛型不回退注册字体——与 css_text.rs 同前置）。
fn engine_ua(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.add_font(FONT.to_vec());
    engine.set_ua_stylesheet(DEFAULT_UA_SHEET);
    let base = "#root { font-family: \"DejaVu Sans\"; }";
    let full = if sheet.is_empty() {
        base.to_string()
    } else {
        format!("{base} {sheet}")
    };
    engine.set_stylesheet(&full);
    let root = StyleNode {
        id: Some("root".to_string()),
        ..StyleNode::default()
    };
    engine.insert(None, 1, root).unwrap();
    engine
}

fn li(parent: u64, key: u64, text: &str) -> StyleNode {
    let _ = parent;
    StyleNode {
        id: Some(key.to_string()),
        name: Some("li".into()),
        text: Some(text.to_string()),
        ..StyleNode::default()
    }
}

/// 本帧全部 Text op 的 (text, [r,g,b,a])（树序）。
fn text_ops(e: &mut StyleEngine<u64>) -> Vec<(String, [f32; 4])> {
    let fr = e.frame((800.0, 600.0), 1.0, 0.0);
    fr.paint
        .ops
        .iter()
        .filter_map(|op| match op {
            style_engine::PaintOp::Text { text, color, .. } => {
                Some((text.clone(), color.components))
            }
            _ => None,
        })
        .collect()
}

fn box_of(e: &mut StyleEngine<u64>, key: u64) -> (f32, f32, f32, f32) {
    let fr = e.frame((800.0, 600.0), 1.0, 0.0);
    let b = fr.find(key).unwrap();
    (b.x, b.y, b.width, b.height)
}

#[test]
fn ol_decimal_numbering_and_implicit_counter() {
    // UA 表 ol{decimal}：三个 li → marker "1. "/"2. "/"3. "（隐式
    // list-item 计数器逐项 +1，§4.6），且 marker op 在宿主文本 op 之前。
    let mut e = engine_ua("");
    e.insert(
        Some(1),
        2,
        StyleNode {
            id: Some("2".into()),
            name: Some("ol".into()),
            ..StyleNode::default()
        },
    )
    .unwrap();
    e.insert(Some(2), 3, li(2, 3, "first")).unwrap();
    e.insert(Some(2), 4, li(2, 4, "second")).unwrap();
    e.insert(Some(2), 5, li(2, 5, "third")).unwrap();
    let ops = text_ops(&mut e);
    let texts: Vec<&str> = ops.iter().map(|(t, _)| t.as_str()).collect();
    assert!(
        texts.contains(&"1. "),
        "首项 marker '1. '（实际 {texts:?}）"
    );
    assert!(
        texts.contains(&"2. "),
        "次项 marker '2. '（实际 {texts:?}）"
    );
    assert!(
        texts.contains(&"3. "),
        "三项 marker '3. '（实际 {texts:?}）"
    );
    // 树序：每项 marker 在其宿主文本前。
    let i1 = texts.iter().position(|t| *t == "1. ").unwrap();
    let f1 = texts.iter().position(|t| *t == "first").unwrap();
    assert!(i1 < f1, "marker 先于宿主文本（{texts:?}）");
}

#[test]
fn ul_disc_and_nested_circle() {
    // UA 表 ul{disc}、ul ul{circle}：外层 "• "、内层 "◦ "（嵌套换族）。
    let mut e = engine_ua("");
    e.insert(
        Some(1),
        2,
        StyleNode {
            id: Some("2".into()),
            name: Some("ul".into()),
            ..StyleNode::default()
        },
    )
    .unwrap();
    e.insert(Some(2), 3, li(2, 3, "outer")).unwrap();
    e.insert(
        Some(3),
        4,
        StyleNode {
            id: Some("4".into()),
            name: Some("ul".into()),
            ..StyleNode::default()
        },
    )
    .unwrap();
    e.insert(Some(4), 5, li(4, 5, "inner")).unwrap();
    let texts: Vec<String> = text_ops(&mut e).into_iter().map(|(t, _)| t).collect();
    assert!(texts.iter().any(|t| t == "• "), "外层 disc（{texts:?}）");
    assert!(texts.iter().any(|t| t == "◦ "), "内层 circle（{texts:?}）");
}

#[test]
fn list_style_none_suppresses_marker() {
    // list-style-type: none（简写 `list-style: none` 同面）→ 无 marker 文本。
    let mut e = engine_ua("li { list-style: none; }");
    e.insert(
        Some(1),
        2,
        StyleNode {
            id: Some("2".into()),
            name: Some("ul".into()),
            ..StyleNode::default()
        },
    )
    .unwrap();
    e.insert(Some(2), 3, li(2, 3, "item")).unwrap();
    let texts: Vec<String> = text_ops(&mut e).into_iter().map(|(t, _)| t).collect();
    assert!(
        !texts.iter().any(|t| t == "• "),
        "none 抑制标记（{texts:?}）"
    );
    assert!(texts.iter().any(|t| t == "item"), "宿主文本仍在");
}

#[test]
fn string_type_literal_marker() {
    // <string> 形 = 字面（无 prefix/suffix 附加）。
    let mut e = engine_ua("li { list-style-type: \"->\"; }");
    e.insert(
        Some(1),
        2,
        StyleNode {
            id: Some("2".into()),
            name: Some("ul".into()),
            ..StyleNode::default()
        },
    )
    .unwrap();
    e.insert(Some(2), 3, li(2, 3, "item")).unwrap();
    let texts: Vec<String> = text_ops(&mut e).into_iter().map(|(t, _)| t).collect();
    assert!(texts.iter().any(|t| t == "->"), "字面标记（{texts:?}）");
}

#[test]
fn author_marker_color_applies() {
    // li::marker { color: red }——伪元素选择器经 originating element 命中
    // marker 伪节点；宿主文本仍继承默认色。
    let mut e = engine_ua("li::marker { color: #ff0000; }");
    e.insert(
        Some(1),
        2,
        StyleNode {
            id: Some("2".into()),
            name: Some("ol".into()),
            ..StyleNode::default()
        },
    )
    .unwrap();
    e.insert(Some(2), 3, li(2, 3, "item")).unwrap();
    let ops = text_ops(&mut e);
    let marker = ops.iter().find(|(t, _)| t == "1. ").expect("marker op");
    assert!(
        (marker.1[0] - 1.0).abs() < 1e-3 && marker.1[1].abs() < 1e-3 && marker.1[2].abs() < 1e-3,
        "marker 红（{:?}）",
        marker.1
    );
    let host = ops.iter().find(|(t, _)| t == "item").expect("host op");
    assert!(
        host.1[0].abs() < 1e-3,
        "宿主文本不受 marker 规则染色（{:?}）",
        host.1
    );
}

#[test]
fn display_list_item_on_div_and_counter_reset() {
    // display:list-item 任意元素生成 marker；counter-reset: list-item N
    // 显式基准 + 隐式 +1（css-lists-3 §4.6 与 §5 计数器交互）。
    let mut e = engine_ua(
        "#a { display: list-item; list-style-type: decimal; counter-reset: list-item 4; } \
                           #b { display: list-item; list-style-type: decimal; }",
    );
    e.insert(
        Some(1),
        2,
        StyleNode {
            id: Some("a".into()),
            name: Some("div".into()),
            text: Some("alpha".to_string()),
            ..StyleNode::default()
        },
    )
    .unwrap();
    e.insert(
        Some(1),
        3,
        StyleNode {
            id: Some("b".into()),
            name: Some("div".into()),
            text: Some("beta".to_string()),
            ..StyleNode::default()
        },
    )
    .unwrap();
    let texts: Vec<String> = text_ops(&mut e).into_iter().map(|(t, _)| t).collect();
    assert!(
        texts.iter().any(|t| t == "5. "),
        "reset 4 + 隐式 1 = 5（{texts:?}）"
    );
    assert!(
        texts.iter().any(|t| t == "6. "),
        "兄弟继承 5 + 1 = 6（{texts:?}）"
    );
}

#[test]
fn marker_layout_inside_first_line() {
    // inside 语义近似：marker 文本 op x = 宿主盒内容左缘（首行打包）；
    // 宿主文本 op x 退让 marker 宽度之后。
    let mut e = engine_ua("");
    e.insert(
        Some(1),
        2,
        StyleNode {
            id: Some("2".into()),
            name: Some("ol".into()),
            ..StyleNode::default()
        },
    )
    .unwrap();
    e.insert(Some(2), 3, li(2, 3, "item")).unwrap();
    e.insert(Some(2), 4, li(2, 4, "second")).unwrap();
    let fr = e.frame((800.0, 600.0), 1.0, 0.0);
    let li_box = fr.find(3).unwrap();
    let mut marker_x = None;
    let mut host_x = None;
    for op in &fr.paint.ops {
        if let style_engine::PaintOp::Text { text, x, .. } = op {
            if text == "1. " {
                marker_x = Some(*x);
            }
            if text == "item" {
                host_x = Some(*x);
            }
        }
    }
    let mx = marker_x.expect("marker op");
    let hx = host_x.expect("host op");
    assert!(
        (mx - li_box.x).abs() < 1.0,
        "marker 起于宿主内容左缘（mx={mx} box.x={}）",
        li_box.x
    );
    assert!(hx > mx, "宿主文本退让 marker 宽度（hx={hx} mx={mx}）");
    // 纵向：两 li 堆叠（marker 不破坏块流）。
    let (x1, y1, _w1, _h1) = box_of(&mut e, 3);
    let (_x2, y2, _w2, _h2) = box_of(&mut e, 4);
    assert_eq!(x1, 0.0);
    assert!(y2 > y1, "li 逐项堆叠（y1={y1} y2={y2}）");
}

#[test]
fn marker_frame_stable_and_differential_safe() {
    // 稳态幂等：连续两帧 marker 文本与几何一致（materialize/suppress 每
    // 帧重放不漂移）。
    let mut e = engine_ua("");
    e.insert(
        Some(1),
        2,
        StyleNode {
            id: Some("2".into()),
            name: Some("ol".into()),
            ..StyleNode::default()
        },
    )
    .unwrap();
    e.insert(Some(2), 3, li(2, 3, "stable")).unwrap();
    let a = {
        let fr = e.frame((800.0, 600.0), 1.0, 0.0);
        (
            fr.paint
                .ops
                .iter()
                .filter_map(|op| match op {
                    style_engine::PaintOp::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            fr.find(3).map(|b| (b.x, b.y, b.width, b.height)),
        )
    };
    let b = {
        let fr = e.frame((800.0, 600.0), 1.0, 0.0);
        (
            fr.paint
                .ops
                .iter()
                .filter_map(|op| match op {
                    style_engine::PaintOp::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            fr.find(3).map(|b| (b.x, b.y, b.width, b.height)),
        )
    };
    assert_eq!(a, b, "两帧逐位一致");
    assert!(a.0.iter().any(|t| t == "1. "));
}

#[test]
fn non_list_item_marker_suppressed() {
    // 非 list-item 宿主的 ::marker 内容计算为 none（§3.1）——普通 div
    // 不产生 marker 文本 op（节点存在但抑制成盒）。
    let mut e = engine_ua("");
    e.insert(
        Some(1),
        2,
        StyleNode {
            id: Some("2".into()),
            name: Some("div".into()),
            text: Some("plain".to_string()),
            ..StyleNode::default()
        },
    )
    .unwrap();
    let texts: Vec<String> = text_ops(&mut e).into_iter().map(|(t, _)| t).collect();
    assert_eq!(texts, vec!["plain".to_string()], "仅宿主文本（{texts:?}）");
}
