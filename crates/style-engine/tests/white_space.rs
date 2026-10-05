//! A5 white-space pre-wrap / pre-line / break-spaces 锁测试。
//!
//! 语义矩阵（css-text-3/css-text-decor）：normal=折叠+按宽换行；
//! nowrap=折叠+不换行；pre=保留+不换行；pre-wrap=保留+按宽换行；
//! pre-line=折叠空格但保留换行符+按宽换行；break-spaces=保留+按宽换行
//!（任意字符断行=v1 常规断点近似，B 级在案 FEATURES.md）。
//! 粘滞前提：T5c-2 重排 pass 的换行判定已扩为
//! None|Normal|PreWrap|PreLine|BreakSpaces（engine.rs）。

#![cfg(feature = "text")]

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

const FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../style-engine-demo/assets/fonts/DejaVuSans.ttf"
));

fn frame_text_height(
    sheet: &str,
    text: &str,
    width: f32,
) -> (f32, style_engine::css::property::WhiteSpace) {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.add_font(FONT.to_vec());
    engine.set_stylesheet(sheet);
    let mut doc = StyleNode::default();
    doc.id = Some("doc".to_string());
    engine.insert(None, 1, doc).unwrap();
    let mut leaf = StyleNode::default();
    leaf.id = Some("t".to_string());
    leaf.text = Some(text.to_string());
    engine.insert(Some(1), 2, leaf).unwrap();
    let f = engine.frame((width, 600.0), 1.0, 0.0);
    let b = f.boxes.iter().find(|b| b.key == 2).unwrap();
    let cs = engine.computed_style(2).unwrap();
    (b.height, cs.white_space())
}

fn two_leaf_sheet(ws: &str) -> String {
    // 显式注册族名——未注册字体对应的泛族（缺省 sans-serif）测量为 0 尺寸
    //（FEATURES.md 契约），必须显式指定已注册族名。
    format!(
        "#doc {{ width: {{W}}px; }} #t {{ white-space: {ws}; font-size: 16px; font-family: \"DejaVu Sans\"; }}"
    )
}

fn height_with(
    ws: &str,
    text: &str,
    doc_width: f32,
) -> (f32, style_engine::css::property::WhiteSpace) {
    let sheet = two_leaf_sheet(ws).replace("{W}", &format!("{}", doc_width as u32));
    frame_text_height(&sheet, text, doc_width)
}

#[test]
fn pre_wrap_wraps_and_preserves() {
    // 长空白串（保留语义下不可折叠，宽度足够触发换行）
    let text = "aaaa        bbbb        cccc";
    let (narrow, ws) = height_with("pre-wrap", text, 120.0);
    assert_eq!(ws, style_engine::css::property::WhiteSpace::PreWrap);
    let (wide, _) = height_with("pre-wrap", text, 600.0);
    assert!(
        narrow > wide * 1.4,
        "pre-wrap 窄盒应换行（narrow={narrow} wide={wide}）"
    );
}

#[test]
fn pre_line_preserves_newlines_collapses_spaces() {
    // 换行符保留：两行 > 一行（窄盒也不因空格折叠而差异）
    let (two, ws) = height_with("pre-line", "line1\nline2", 400.0);
    assert_eq!(ws, style_engine::css::property::WhiteSpace::PreLine);
    let (one, _) = height_with("pre-line", "line1", 400.0);
    assert!(
        two > one * 1.4,
        "pre-line 应保留换行符（two={two} one={one}）"
    );
    // 空格折叠：宽盒内长空格串不产生第二行（与 pre-wrap 相反）
    let (collapsed, _) = height_with("pre-line", "aa        bb", 600.0);
    let (single, _) = height_with("pre-line", "aabb", 600.0);
    assert!(
        (collapsed - single).abs() < 1.0,
        "pre-line 应折叠空格（collapsed={collapsed} single={single}）"
    );
}

#[test]
fn break_spaces_wraps_like_pre_wrap() {
    let text = "aaaa        bbbb        cccc";
    let (narrow, ws) = height_with("break-spaces", text, 120.0);
    assert_eq!(ws, style_engine::css::property::WhiteSpace::BreakSpaces);
    let (wide, _) = height_with("break-spaces", text, 600.0);
    assert!(narrow > wide * 1.4, "break-spaces 窄盒应换行");
}

#[test]
fn pre_and_nowrap_never_wrap() {
    let text = "aaaa        bbbb        cccc";
    let (narrow, _) = height_with("pre", text, 80.0);
    let (wide, _) = height_with("pre", text, 600.0);
    assert!(
        (narrow - wide).abs() < 1.0,
        "pre 不换行（narrow={narrow} wide={wide}）"
    );
    let (n2, ws2) = height_with("nowrap", text, 80.0);
    let (w2, _) = height_with("nowrap", text, 600.0);
    assert_eq!(ws2, style_engine::css::property::WhiteSpace::NoWrap);
    assert!((n2 - w2).abs() < 1.0, "nowrap 不换行");
}

#[test]
fn normal_collapses_and_wraps_baseline() {
    let (narrow, ws) = height_with("normal", "aaaa        bbbb", 90.0);
    assert_eq!(ws, style_engine::css::property::WhiteSpace::Normal);
    let (wide, _) = height_with("normal", "aaaa        bbbb", 600.0);
    assert!(narrow > wide, "normal 折叠后仍按宽换行");
}
