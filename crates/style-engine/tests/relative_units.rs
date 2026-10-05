//! A9 容器/字体相对单位锁测试：cqw/cqh/cqi（延迟结算 + 容器基值结算期
//! 现查）、无容器祖先回落 small viewport（规范缺省）、calc 混排
//!（calc(50cqw + 10px)）、ch/ex/ic 字体相对单位（注册字体真度量 vs
//! 未注册近似缺省；ic 缺字=1em 契约）。

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

const DEJAVU: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../style-engine-demo/assets/fonts/DejaVuSans.ttf"
));

/// 结构：根(#root,id=1) → 容器(#c,id=2) → 目标(#t,id=3)；返回 #t 盒几何。
fn box_in_container(sheet: &str, viewport: (f32, f32)) -> (f32, f32, f32, f32) {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut c = StyleNode::default();
    c.id = Some("c".to_string());
    engine.insert(Some(1), 2, c).unwrap();
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
    engine.insert(Some(2), 3, t).unwrap();
    let f = engine.frame(viewport, 1.0, 0.0);
    let b = f.boxes.iter().find(|b| b.key == 3).unwrap();
    (b.x, b.y, b.width, b.height)
}

/// 结构：根(#root,id=1) → 目标(#t,id=2)（无容器祖先路径）。
fn box_flat(sheet: &str, viewport: (f32, f32)) -> (f32, f32, f32, f32) {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
    engine.insert(Some(1), 2, t).unwrap();
    let f = engine.frame(viewport, 1.0, 0.0);
    let b = f.boxes.iter().find(|b| b.key == 2).unwrap();
    (b.x, b.y, b.width, b.height)
}

const CONTAINER: &str = "#c { container-type: size; width: 400px; height: 200px; }";

#[test]
fn cqw_uses_container_not_viewport() {
    // 视口 (800,600)、容器 400：50cqw = 200（若误用视口回落则 400）
    let (_, _, w, _) = box_in_container(
        &format!("{CONTAINER} #t {{ width: 50cqw; height: 20px; }}"),
        (800.0, 600.0),
    );
    assert!((w - 200.0).abs() < 0.5, "50cqw 应=容器宽 200，实际 {w}");
}

#[test]
fn cqh_uses_container_height() {
    let (_, _, _, h) = box_in_container(
        &format!("{CONTAINER} #t {{ width: 20px; height: 50cqh; }}"),
        (800.0, 600.0),
    );
    assert!((h - 100.0).abs() < 0.5, "50cqh 应=容器高 100，实际 {h}");
}

#[test]
fn cqb_matches_cqh_block_axis() {
    let (_, _, _, h) = box_in_container(
        &format!("{CONTAINER} #t {{ width: 20px; height: 25cqb; }}"),
        (800.0, 600.0),
    );
    assert!(
        (h - 50.0).abs() < 0.5,
        "25cqb 应=容器高 50（水平书写），实际 {h}"
    );
}

#[test]
fn cqi_matches_cqw_inline_axis() {
    let (_, _, w, _) = box_in_container(
        &format!("{CONTAINER} #t {{ width: 25cqi; height: 20px; }}"),
        (800.0, 600.0),
    );
    assert!(
        (w - 100.0).abs() < 0.5,
        "25cqi 应=容器宽 100（水平书写），实际 {w}"
    );
}

#[test]
fn calc_mixes_cq_and_px() {
    let (_, _, w, _) = box_in_container(
        &format!("{CONTAINER} #t {{ width: calc(50cqw + 10px); height: 20px; }}"),
        (800.0, 600.0),
    );
    assert!(
        (w - 210.0).abs() < 0.5,
        "calc(50cqw + 10px) 应=210，实际 {w}"
    );
}

#[test]
fn no_container_falls_back_to_small_viewport() {
    // 无容器祖先：cq 单位=small viewport（规范回落）
    let (_, _, w, _) = box_flat("#t { width: 25cqw; height: 20px; }", (800.0, 600.0));
    assert!(
        (w - 200.0).abs() < 0.5,
        "无容器 25cqw 应=视口宽 200，实际 {w}"
    );
    let (_, _, _, h) = box_flat("#t { width: 20px; height: 25cqh; }", (800.0, 600.0));
    assert!(
        (h - 150.0).abs() < 0.5,
        "无容器 25cqh 应=视口高 150，实际 {h}"
    );
}

#[test]
fn ch_uses_registered_font_metrics() {
    // 注册 DejaVu：ch = 数字 0 advance（≈0.636em）→ 10ch@20px ≈ 127（≠0.5em 近似 100）
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.add_font(DEJAVU.to_vec());
    engine.set_stylesheet(
        "#t { width: 10ch; height: 20px; font-size: 20px; font-family: \"DejaVu Sans\"; }",
    );
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
    engine.insert(Some(1), 2, t).unwrap();
    let f = engine.frame((800.0, 600.0), 1.0, 0.0);
    let w = f.boxes.iter().find(|b| b.key == 2).unwrap().width;
    assert!(
        w > 101.0 && w < 200.0,
        "10ch@20px 应取注册字体真度量（≈127，非 0.5em 近似 100），实际 {w}"
    );
    // 对照：未注册字体 → 0.5em 近似 = 100.0
    let (_, _, w0, _) = box_flat(
        "#t { width: 10ch; height: 20px; font-size: 20px; font-family: \"No Such Family\"; }",
        (800.0, 600.0),
    );
    assert!(
        (w0 - 100.0).abs() < 0.5,
        "未注册族 10ch 应=0.5em 近似 100，实际 {w0}"
    );
}

#[test]
fn ex_uses_x_height_metrics() {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.add_font(DEJAVU.to_vec());
    engine.set_stylesheet(
        "#t { width: 10ex; height: 20px; font-size: 20px; font-family: \"DejaVu Sans\"; }",
    );
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
    engine.insert(Some(1), 2, t).unwrap();
    let f = engine.frame((800.0, 600.0), 1.0, 0.0);
    let w = f.boxes.iter().find(|b| b.key == 2).unwrap().width;
    assert!(
        w > 101.0 && w < 200.0,
        "10ex@20px 应取真 x-height（≈109，非 0.5em 近似 100），实际 {w}"
    );
}

#[test]
fn ic_falls_back_to_1em_when_glyph_missing() {
    // DejaVu 无 U+6C34（水）→ ic=1.0em：5ic@20px = 100（缺字契约锁）
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.add_font(DEJAVU.to_vec());
    engine.set_stylesheet(
        "#t { width: 5ic; height: 20px; font-size: 20px; font-family: \"DejaVu Sans\"; }",
    );
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
    engine.insert(Some(1), 2, t).unwrap();
    let f = engine.frame((800.0, 600.0), 1.0, 0.0);
    let w = f.boxes.iter().find(|b| b.key == 2).unwrap().width;
    assert!(
        (w - 100.0).abs() < 0.5,
        "5ic@20px 缺字应=1em 基准 100，实际 {w}"
    );
}
