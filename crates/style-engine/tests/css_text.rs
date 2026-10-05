//! C2 text-transform / overflow-wrap / word-break 锁测试（css-text-3 /
//! ADR-0016）。
//!
//! 度量尺 = 折行几何（窄容器高度比较）：text-transform 改变字形宽 → 折行
//! 数变化；word-break/overflow-wrap 经 parley 原生段器改变断点。CJK/
//! full-width 用例用 Noto Sans SC（DejaVu 无 CJK/全角字形覆盖）。

#![cfg(feature = "text")]

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

const FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../style-engine-demo/assets/fonts/DejaVuSans.ttf"
));
const FONT_SC: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../style-engine-demo/assets/fonts/NotoSansSC.ttf"
));

/// 双节点骨架：#root(1) → #t(2)，#t 携带文本；双字体入库。
fn engine_text(sheet: &str, text: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.add_font(FONT.to_vec());
    engine.add_font(FONT_SC.to_vec());
    engine.set_stylesheet(sheet);
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut leaf = StyleNode::default();
    leaf.id = Some("t".to_string());
    leaf.text = Some(text.to_string());
    engine.insert(Some(1), 2, leaf).unwrap();
    engine
}

/// #t 盒高。
fn box_h(e: &mut StyleEngine<u64>) -> f32 {
    let f = e.frame((800.0, 600.0), 1.0, 0.0);
    f.boxes.iter().find(|b| b.key == 2).unwrap().height
}

const W: &str = "#root { width: 60px; }";
const BASE: &str = "font-size: 16px; font-family: \"DejaVu Sans\";";
const L30: &str = "llllllllllllllllllllllllllllll"; // 30×l（单词：等值断言用）
const S30: &str = "lll lll lll lll lll lll lll lll lll lll"; // 10 词×3l（可折行）
const A20: &str = "aaaaaaaaaaaaaaaaaaaa"; // 20×a（单不可断词）

#[test]
fn uppercase_wraps_taller_than_lowercase() {
    // 纯单词在 normal white-space 下不可断（溢出单行），带空格才能比较折行几何。
    let mut a = engine_text(
        &format!("{W} #t {{ {BASE} text-transform: lowercase; }}"),
        S30,
    );
    let h_low = box_h(&mut a);
    let mut b = engine_text(
        &format!("{W} #t {{ {BASE} text-transform: uppercase; }}"),
        S30,
    );
    let h_up = box_h(&mut b);
    assert!(
        h_low > 0.0 && h_up > h_low,
        "L 宽于 l → 折更多行（low={h_low} up={h_up}）"
    );
}

#[test]
fn capitalize_widens_word_starts() {
    let text = "ll ll ll ll ll ll ll ll ll ll"; // 10 词
    let mut a = engine_text(
        &format!("{W} #t {{ {BASE} text-transform: lowercase; }}"),
        text,
    );
    let h_low = box_h(&mut a);
    let mut b = engine_text(
        &format!("{W} #t {{ {BASE} text-transform: capitalize; }}"),
        text,
    );
    let h_cap = box_h(&mut b);
    assert!(h_cap > h_low, "词首大写加宽（low={h_low} cap={h_cap}）");
}

#[test]
fn text_transform_inherits() {
    let mut a = engine_text(
        &format!("{W} #t {{ {BASE} text-transform: uppercase; }}"),
        L30,
    );
    let h_direct = box_h(&mut a);
    let mut b = engine_text(
        &format!("{W} #root {{ text-transform: uppercase; }} #t {{ {BASE} }}"),
        L30,
    );
    let h_inherit = box_h(&mut b);
    assert!(
        (h_direct - h_inherit).abs() < 0.5,
        "继承与直设等高（direct={h_direct} inherit={h_inherit}）"
    );
}

#[test]
fn full_size_kana_inert_for_latin() {
    // T2 契约（ADR-0016）：接受但不变换 → 与 none 等高
    let mut a = engine_text(&format!("{W} #t {{ {BASE} }}"), L30);
    let h0 = box_h(&mut a);
    let mut b = engine_text(
        &format!("{W} #t {{ {BASE} text-transform: full-size-kana; }}"),
        L30,
    );
    let h1 = box_h(&mut b);
    assert!(
        (h0 - h1).abs() < 0.5,
        "full-size-kana 对拉丁不变换（{h0} vs {h1}）"
    );
}

#[test]
fn full_width_expands_latin() {
    let text = "llllllllllllllllllll"; // 20×l
    let sc = "font-size: 16px; font-family: \"Noto Sans SC\";";
    let mut a = engine_text(&format!("#root {{ width: 60px; }} #t {{ {sc} }}"), text);
    let h0 = box_h(&mut a);
    let mut b = engine_text(
        &format!("#root {{ width: 60px; }} #t {{ {sc} text-transform: full-width; }}"),
        text,
    );
    let h1 = box_h(&mut b);
    assert!(h0 > 0.0 && h1 > h0, "全角加宽 → 折更多行（{h0} → {h1}）");
}

#[test]
fn word_break_break_all_wraps_midword() {
    let mut a = engine_text(&format!("{W} #t {{ {BASE} }}"), A20);
    let h_n = box_h(&mut a);
    let mut b = engine_text(&format!("{W} #t {{ {BASE} word-break: break-all; }}"), A20);
    let h_all = box_h(&mut b);
    assert!(
        h_n > 0.0 && h_all > h_n * 1.5,
        "break-all 词内断行（norm={h_n} all={h_all}）"
    );
}

#[test]
fn overflow_wrap_break_word_wraps_overflow() {
    let mut a = engine_text(&format!("{W} #t {{ {BASE} }}"), A20);
    let h_n = box_h(&mut a);
    let mut b = engine_text(
        &format!("{W} #t {{ {BASE} overflow-wrap: break-word; }}"),
        A20,
    );
    let h_bw = box_h(&mut b);
    assert!(
        h_n > 0.0 && h_bw > h_n * 1.5,
        "break-word 溢出断行（norm={h_n} bw={h_bw}）"
    );
}

#[test]
fn overflow_wrap_anywhere_matches_break_word_height() {
    let mut a = engine_text(
        &format!("{W} #t {{ {BASE} overflow-wrap: break-word; }}"),
        A20,
    );
    let h_bw = box_h(&mut a);
    let mut b = engine_text(
        &format!("{W} #t {{ {BASE} overflow-wrap: anywhere; }}"),
        A20,
    );
    let h_an = box_h(&mut b);
    assert!(
        (h_bw - h_an).abs() < 0.5,
        "折行几何同（bw={h_bw} anywhere={h_an}）"
    );
}

#[test]
fn word_wrap_legacy_alias_matches() {
    let mut a = engine_text(
        &format!("{W} #t {{ {BASE} overflow-wrap: break-word; }}"),
        A20,
    );
    let h_direct = box_h(&mut a);
    let mut b = engine_text(&format!("{W} #t {{ {BASE} word-wrap: break-word; }}"), A20);
    let h_alias = box_h(&mut b);
    assert!(
        (h_direct - h_alias).abs() < 0.5,
        "word-wrap 别名同 overflow-wrap（{h_direct} vs {h_alias}）"
    );
}

#[test]
fn word_break_keep_all_forbids_cjk_breaks() {
    let text = "你好世界你好世界你好世界"; // 12 CJK，无空格
    let sc = "font-size: 16px; font-family: \"Noto Sans SC\";";
    let mut a = engine_text(&format!("#root {{ width: 60px; }} #t {{ {sc} }}"), text);
    let h_n = box_h(&mut a);
    let mut b = engine_text(
        &format!("#root {{ width: 60px; }} #t {{ {sc} word-break: keep-all; }}"),
        text,
    );
    let h_k = box_h(&mut b);
    assert!(
        h_n > 0.0 && h_k < h_n,
        "keep-all 禁词内断（norm={h_n} keep={h_k}）"
    );
}

#[test]
fn uppercase_and_break_all_interplay() {
    let mut a = engine_text(&format!("{W} #t {{ {BASE} word-break: break-all; }}"), L30);
    let h1 = box_h(&mut a);
    let mut b = engine_text(
        &format!("{W} #t {{ {BASE} text-transform: uppercase; word-break: break-all; }}"),
        L30,
    );
    let h2 = box_h(&mut b);
    assert!(h2 > h1, "变换+断行叠加（{h1} → {h2}）");
}
