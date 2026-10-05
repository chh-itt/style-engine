//! 像素不变量锁（F3e 切片③，ADR-0027 D4）：软 Sink 光栅化的
//! 区域级 RGBA 语义锁——不依赖 Chromium golden（golden 截图采集
//! 需浏览器环境，本地不可得；保持 manifest 通道 `[pixel]` 现状），
//! 改为对 F3b/F3c/F3d 绘制面直接断言像素统计不变量：
//!
//! | 锁 | 面 |
//! |----|----|
//! | gradient_axis_monotonic | 线性渐变轴向单调（F3b） |
//! | background_layer_order_first_on_top | 多层背景首层在最上（F3b） |
//! | clip_path_circle_masks_corners | clip-path 圆形遮罩角部（F3c） |
//! | border_image_paints_ring_only | border-image 九宫格描环不填心（F3d） |
//! | hard_box_shadow_offset_ink | 硬阴影偏移墨迹（F3b 时代面复锁） |
//! | opacity_half_blend | 0.5 不透明度白底混色 |
//! | rendering_is_deterministic | 同输入逐字节确定性 |
//!
//! 通道分工不变：Chromium golden 对比仍走 tests/pixel.rs（manifest
//! `[pixel]`）；本文件为引擎自身的像素语义回归网。

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;
use style_engine_soft::FontBank;

/// 单元素页面：视口 120×120、根 div 100×100 @ (10,10)（margin 定位）、
/// 软 Sink 白底光栅化。
fn render(css: &str) -> style_engine_soft::SoftCanvas {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(css);
    let mut root_node = StyleNode::default();
    root_node.id = Some("root".to_string());
    engine.insert(None, 1, root_node).unwrap();
    let frame = engine.frame((120.0, 120.0), 1.0, 0.0);
    style_engine_soft::render_with_fonts(
        &frame.paint,
        120,
        120,
        [255, 255, 255, 255],
        &FontBank::new(),
    )
}

fn px(c: &style_engine_soft::SoftCanvas, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * c.width + x) * 4) as usize;
    [
        c.pixels[i],
        c.pixels[i + 1],
        c.pixels[i + 2],
        c.pixels[i + 3],
    ]
}

fn is_white(p: [u8; 4]) -> bool {
    p[0] > 250 && p[1] > 250 && p[2] > 250
}

#[test]
fn gradient_axis_monotonic() {
    let c = render(
        "#root { margin: 10px; width: 100px; height: 100px; \
         background-image: linear-gradient(to bottom, red, blue); }",
    );
    let top = px(&c, 60, 15);
    let mid = px(&c, 60, 60);
    let bot = px(&c, 60, 105);
    assert!(top[0] > 200 && top[2] < 60, "顶部应近红，实际 {top:?}");
    assert!(bot[2] > 200 && bot[0] < 60, "底部应近蓝，实际 {bot:?}");
    assert!(
        (mid[0] as i32 - mid[2] as i32).abs() < 60,
        "中点应近紫（R≈B），实际 {mid:?}"
    );
}

#[test]
fn background_layer_order_first_on_top() {
    // 首层（红→蓝）盖住第二层（绿→黄）：中点 G 必须≈0（第二层中点 G≈255）。
    let c = render(
        "#root { margin: 10px; width: 100px; height: 100px; \
         background-image: linear-gradient(red, blue), linear-gradient(green, yellow); }",
    );
    let mid = px(&c, 60, 60);
    assert!(mid[1] < 100, "首层应最上（中点非绿），实际 {mid:?}");
}

#[test]
fn clip_path_circle_masks_corners() {
    let c = render(
        "#root { margin: 10px; width: 100px; height: 100px; background-color: red; \
         clip-path: circle(50% at 50% 50%); }",
    );
    let center = px(&c, 60, 60);
    let near_top = px(&c, 60, 15);
    let corner = px(&c, 13, 13);
    assert!(
        center[0] > 200 && center[1] < 60,
        "圆心应红，实际 {center:?}"
    );
    assert!(
        near_top[0] > 200,
        "圆内近顶点 (60,15) 距心 45px < 50 应红，实际 {near_top:?}"
    );
    assert!(
        is_white(corner),
        "盒角 (13,13) 距心 66px > 50 应被遮罩为白，实际 {corner:?}"
    );
}

#[test]
fn border_image_paints_ring_only() {
    let c = render(
        "#root { margin: 10px; width: 100px; height: 100px; background: white; \
         border-image: linear-gradient(to bottom, red, blue) 30 / 10px; }",
    );
    let top_band = px(&c, 60, 15);
    let left_band = px(&c, 15, 60);
    let corner = px(&c, 15, 15);
    let center = px(&c, 60, 60);
    let outside = px(&c, 5, 60);
    for (label, p) in [
        ("顶边带", top_band),
        ("左边带", left_band),
        ("角块", corner),
    ] {
        assert!(
            !is_white(p),
            "{label} 应被 border-image 墨迹覆盖，实际 {p:?}"
        );
    }
    assert!(
        is_white(center),
        "无 fill 关键字中心不应绘制，实际 {center:?}"
    );
    assert!(is_white(outside), "盒外应白，实际 {outside:?}");
}

#[test]
fn hard_box_shadow_offset_ink() {
    let c = render(
        "#root { margin: 10px; width: 100px; height: 100px; background: blue; \
         box-shadow: 20px 20px 0 0 red; }",
    );
    let on_box = px(&c, 60, 60);
    let shadow_only = px(&c, 118, 118);
    let outside = px(&c, 5, 5);
    assert!(
        on_box[2] > 200 && on_box[0] < 60,
        "盒面应蓝，实际 {on_box:?}"
    );
    assert!(
        shadow_only[0] > 200 && shadow_only[2] < 60,
        "纯阴影区 (118,118) 应红，实际 {shadow_only:?}"
    );
    assert!(is_white(outside), "两区之外应白，实际 {outside:?}");
}

#[test]
fn opacity_half_blend() {
    let c = render(
        "#root { margin: 10px; width: 100px; height: 100px; background: red; opacity: 0.5; }",
    );
    let p = px(&c, 60, 60);
    assert!(
        p[0] >= 250 && (115..=140).contains(&p[1]) && (115..=140).contains(&p[2]),
        "0.5 红叠白底应为 (255,127,127)±，实际 {p:?}"
    );
}

#[test]
fn rendering_is_deterministic() {
    let css = "#root { margin: 10px; width: 100px; height: 100px; \
               background: linear-gradient(45deg, red, blue) content-box; \
               box-shadow: 4px 4px 0 0 green; }";
    let a = render(css);
    let b = render(css);
    assert_eq!(a.pixels, b.pixels, "同输入必须逐字节确定");
}
