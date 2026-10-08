//! C4 ::selection / ::placeholder 锁测试（css-pseudo-4 / ADR-0018）。
//!
//! 度量尺 = 通道语义：两伪元素均为非盒生成——origin 节点直配
//! （pseudo == None 即命中），主级联防泄漏（通道规则不进主样式），
//! 继承基 = origin 主样式（css-pseudo-4）；合并表组无对应规则时通道
//! map 整清（零成本）；selection-only 表不触发 materialize_pseudos。

use style_engine::StyleEngine;
use style_engine::css::value::ColorValue;
use style_engine::tree::StyleNode;

/// 双节点骨架：#root(1) → #t(2)。
fn engine_two(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
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

/// 通道色（sel=true 读 ::selection，否则 ::placeholder）——frame 先行
///（样式惰性计算）。
fn channel_rgb(e: &mut StyleEngine<u64>, key: u64, sel: bool) -> [f32; 4] {
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    let s = if sel {
        e.selection_style(key)
            .expect("应有 selection 通道样式")
            .clone()
    } else {
        e.placeholder_style(key)
            .expect("应有 placeholder 通道样式")
            .clone()
    };
    match s.color() {
        ColorValue::Absolute(c) => c.components,
        _ => panic!("应为绝对色"),
    }
}

/// 主样式色（防泄漏断言用）。
fn main_rgb(e: &mut StyleEngine<u64>, key: u64) -> [f32; 4] {
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    match e.computed_style(key).unwrap().color() {
        ColorValue::Absolute(c) => c.components,
        _ => panic!("应为绝对色"),
    }
}

fn approx(a: f32, b: f32) {
    assert!((a - b).abs() < 0.01, "{a} ≈ {b} 失败");
}

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
const LIME: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

// ---------------------------------------------------------------- 通道隔离

#[test]
fn selection_parse_and_channel_isolation() {
    let mut e = engine_two("#root::selection { color: red; } #root { color: blue; }");
    // 通道 = red；主样式不受 ::selection 污染（防泄漏）。
    let sel = channel_rgb(&mut e, 1, true);
    approx(sel[0], RED[0]);
    approx(sel[1], RED[1]);
    approx(sel[2], RED[2]);
    let main = main_rgb(&mut e, 1);
    approx(main[2], BLUE[2]);
    approx(main[0], BLUE[0]);
}

#[test]
fn single_colon_selection_rejected() {
    // :selection 单冒号形非 CSS2 legacy 集 → 选择器解析拒绝 → 规则剔除
    // → 无通道规则 → 通道缺席。
    let mut e = engine_two("#root:selection { color: red; }");
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    assert!(e.selection_style(1).is_none(), "单冒号形须被拒绝");
}

#[test]
fn selection_specificity_ordering() {
    // (1,0,1) > (0,1,1)：#t::selection 胜 .c::selection。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet("#t::selection { color: red; } .c::selection { color: blue; }");
    let root = StyleNode {
        id: Some("root".to_string()),
        ..StyleNode::default()
    };
    e.insert(None, 1, root).unwrap();
    let leaf = StyleNode {
        id: Some("t".to_string()),
        classes: vec!["c".to_string()].into(),
        ..StyleNode::default()
    };
    e.insert(Some(1), 2, leaf).unwrap();
    let sel = channel_rgb(&mut e, 2, true);
    approx(sel[0], RED[0]);
    approx(sel[2], RED[2]);
}

#[test]
fn author_sheet_order_last_wins() {
    // 同 origin 同特异性：追加 author 表（后者胜平手）。
    let mut e = engine_two("#t::selection { color: red; }");
    e.add_stylesheet("#t::selection { color: blue; }");
    let sel = channel_rgb(&mut e, 2, true);
    approx(sel[2], BLUE[2]);
    approx(sel[0], BLUE[0]);
}

#[test]
fn selection_media_gated() {
    let sheet = "@media (min-width: 1000px) { #root::selection { color: red; } }";
    // 800 宽：@media 不命中 → 通道缺席。
    let mut e = engine_two(sheet);
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    assert!(e.selection_style(1).is_none(), "媒体条件外无通道");
    // 1100 宽（独立引擎实例：frame 间无脏标，重样式不重跑 = 引擎既有
    // 视口语义）：命中 → red。
    let mut e2 = engine_two(sheet);
    let _ = e2.frame((1100.0, 700.0), 1.0, 0.0);
    let s = e2.selection_style(1).expect("媒体条件内应有通道").clone();
    match s.color() {
        ColorValue::Absolute(c) => {
            approx(c.components[0], RED[0]);
            approx(c.components[1], RED[1]);
        }
        _ => panic!("应为绝对色"),
    }
}

// ---------------------------------------------------------------- 继承与双通道

#[test]
fn selection_inherits_from_origin() {
    // css-pseudo-4：::selection 继承自 originating element——通道未声明
    // font-size → 取 origin 主样式 20px。
    let mut e = engine_two("#root { font-size: 20px; } #root::selection { color: red; }");
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    let s = e.selection_style(1).expect("应有通道").clone();
    approx(s.font_size_px(), 20.0);
    match s.color() {
        ColorValue::Absolute(c) => approx(c.components[0], RED[0]),
        _ => panic!("应为绝对色"),
    }
}

#[test]
fn placeholder_channel_independent() {
    // 双通道互不污染：selection=red、placeholder=lime。
    let mut e = engine_two("#root::selection { color: red; } #root::placeholder { color: lime; }");
    let sel = channel_rgb(&mut e, 1, true);
    approx(sel[0], RED[0]);
    approx(sel[1], RED[1]);
    let ph = channel_rgb(&mut e, 1, false);
    approx(ph[1], LIME[1]);
    approx(ph[0], LIME[0]);
}

#[test]
fn no_rules_channel_absent() {
    // 无 ::selection 规则 → 通道 map 清除（零成本模式）→ None。
    let mut e = engine_two("#root { color: blue; }");
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    assert!(e.selection_style(1).is_none());
    assert!(e.placeholder_style(1).is_none());
}

// ---------------------------------------------------------------- 实体化闸

/// C1 伪盒语义：content 伪元素合入宿主盒（宿主盒生长），非独立
/// boxes 条目。锁：selection-only 表宿主盒不生长（= plain 基线）；
/// ::before content 宿主盒生长（检测器有效性对照，C1 同形）。
/// 字体度量依赖 → text 特征门（C1 套件同款）。
#[cfg(feature = "text")]
#[test]
fn selection_only_sheet_does_not_materialize() {
    const FONT: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../style-engine-demo/assets/fonts/DejaVuSans.ttf"
    ));
    let font_decl = "#t { font-size: 16px; font-family: \"DejaVu Sans\"; }";
    let host_h = |e: &mut StyleEngine<u64>| {
        let f = e.frame((800.0, 600.0), 1.0, 0.0);
        f.boxes.iter().find(|b| b.key == 2).unwrap().height
    };
    let mut e_plain = engine_two(font_decl);
    e_plain.add_font(FONT.to_vec());
    let h0 = host_h(&mut e_plain);
    let mut e_sel = engine_two(&format!("{font_decl} #root::selection {{ color: red; }}"));
    e_sel.add_font(FONT.to_vec());
    let h1 = host_h(&mut e_sel);
    assert_eq!(h1, h0, "selection-only 表不得实体化伪节点（宿主盒不生长）");
    let mut e_bf = engine_two(&format!("{font_decl} #t::before {{ content: \"Hi\"; }}"));
    e_bf.add_font(FONT.to_vec());
    let h2 = host_h(&mut e_bf);
    assert!(h2 > 0.0, "::before content 应生长宿主盒（对照臂，h={h2}）");
}

#[test]
fn class_prefix_channel_match() {
    // 组合前缀：.sel::selection 仅命中携带该类的节点；无命中 → 通道缺席
    //（winners 双空契约）。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet(".sel::selection { color: red; }");
    let root = StyleNode {
        id: Some("root".to_string()),
        ..StyleNode::default()
    };
    e.insert(None, 1, root).unwrap();
    let leaf = StyleNode {
        id: Some("t".to_string()),
        classes: vec!["sel".to_string()].into(),
        ..StyleNode::default()
    };
    e.insert(Some(1), 2, leaf).unwrap();
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    assert!(e.selection_style(1).is_none(), "无类节点无命中 → 通道缺席");
    let sel2 = channel_rgb(&mut e, 2, true);
    approx(sel2[0], RED[0]);
    approx(sel2[1], RED[1]);
}
