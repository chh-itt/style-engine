//! C3 conic-gradient / object-fit / object-position 锁测试（css-images-3 /
//! ADR-0017）。
//!
//! 度量尺 = 绘制基元几何：元素替换内容经 `Frame.paint` 的 `PaintOp::Image`
//! 落矩形（fit 数学直读），锥形渐变经 `PaintOp::Gradient.conic` 落
//! `ConicGeom`（CSS 0deg=12 点 → peniko 正 X 轴起平移）。源图固定 40×20
//! （2:1 宽高比），内容盒 60×40（无 padding/border → 内容盒=盒）。

use style_engine::StyleEngine;
use style_engine::css::property::{
    BackgroundImage, ColorStop, DeclValue, GradientKind, PropertyId,
};
use style_engine::css::value::{ColorValue, LengthPercentage};
use style_engine::paint::{ConicGeom, PaintOp};
use style_engine::tree::StyleNode;

const SRC_W: u32 = 40;
const SRC_H: u32 = 20;

/// 双节点骨架：#root(1) → #t(2)；#t 携带替换内容引用；`register` 决定
/// 是否注册 40×20 红源（未注册路径验证零副作用告警跳过）。
fn engine_img(sheet: &str, image: Option<&str>, register: bool) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    let mut root = StyleNode::default();
    root.id = Some("root".to_string());
    engine.insert(None, 1, root).unwrap();
    let mut leaf = StyleNode::default();
    leaf.id = Some("t".to_string());
    leaf.image = image.map(|s| s.to_string());
    engine.insert(Some(1), 2, leaf).unwrap();
    if register {
        engine.add_image("k", SRC_W, SRC_H, vec![255; (SRC_W * SRC_H * 4) as usize]);
    }
    engine
}

/// #t 布局盒 (x, y, w, h)。
fn leaf_box(e: &mut StyleEngine<u64>) -> (f32, f32, f32, f32) {
    let f = e.frame((800.0, 600.0), 1.0, 0.0);
    let b = f.find(2).unwrap();
    (b.x, b.y, b.width, b.height)
}

/// 首个 PaintOp::Image 的 dest 矩形 (x, y, w, h)。
fn image_rect(e: &mut StyleEngine<u64>) -> (f32, f32, f32, f32) {
    let f = e.frame((800.0, 600.0), 1.0, 0.0);
    f.paint
        .ops
        .iter()
        .find_map(|op| match op {
            PaintOp::Image {
                x,
                y,
                width,
                height,
                ..
            } => Some((*x, *y, *width, *height)),
            _ => None,
        })
        .expect("应有元素图像 op")
}

/// 首个 PaintOp::Gradient 的 ConicGeom。
fn conic_geom(e: &mut StyleEngine<u64>) -> ConicGeom {
    let f = e.frame((800.0, 600.0), 1.0, 0.0);
    f.paint
        .ops
        .iter()
        .find_map(|op| match op {
            PaintOp::Gradient { conic, .. } => *conic,
            _ => None,
        })
        .expect("应有锥形几何")
}

fn approx(a: f32, b: f32) {
    assert!((a - b).abs() < 0.01, "{a} ≈ {b} 失败");
}

// ---------------------------------------------------------------- conic 解析

#[test]
fn conic_default_from_and_center() {
    let mut e = engine_img(
        "#t { width: 60px; height: 40px; background-image: conic-gradient(red, blue); }",
        None,
        false,
    );
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    let style = e.computed_style(2).unwrap().clone();
    let Some(DeclValue::BackgroundImage(images)) = style.get(PropertyId::BackgroundImage) else {
        panic!("背景图应为渐变");
    };
    let Some(BackgroundImage::Gradient(g)) = images.first() else {
        panic!("背景图应为渐变");
    };
    let GradientKind::Conic(spec) = &g.kind else {
        panic!("应为锥形");
    };
    approx(spec.from.0, 0.0);
    assert_eq!(
        spec.position,
        (
            LengthPercentage::Percent(0.5),
            LengthPercentage::Percent(0.5)
        )
    );
    assert_eq!(g.stops.len(), 2);
    // 默认几何：start = (0 − 90°)·π/180 = −π/2；圆心 = 盒心 (30, 20)。
    let gm = conic_geom(&mut e);
    approx(gm.cx, 30.0);
    approx(gm.cy, 20.0);
    approx(gm.start, -std::f32::consts::FRAC_PI_2);
}

#[test]
fn conic_from_and_at_map_to_peniko_space() {
    // from 90deg → start = 0（正 X 轴）；at 25% 25% → (15, 10)。
    let mut e = engine_img(
        "#t { width: 60px; height: 40px; background-image: conic-gradient(from 90deg at 25% 25%, red, blue); }",
        None,
        false,
    );
    let gm = conic_geom(&mut e);
    approx(gm.cx, 15.0);
    approx(gm.cy, 10.0);
    approx(gm.start, 0.0);
}

#[test]
fn conic_stop_positions_passthrough() {
    let mut e = engine_img(
        "#t { width: 60px; height: 40px; background-image: conic-gradient(red 0%, blue 100%); }",
        None,
        false,
    );
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    let style = e.computed_style(2).unwrap().clone();
    let Some(DeclValue::BackgroundImage(images)) = style.get(PropertyId::BackgroundImage) else {
        panic!("背景图应为渐变");
    };
    let Some(BackgroundImage::Gradient(g)) = images.first() else {
        panic!("背景图应为渐变");
    };
    let stops: &[ColorStop] = &g.stops;
    assert_eq!(stops.len(), 2);
    assert_eq!(stops[0].position, Some(LengthPercentage::Percent(0.0)));
    assert_eq!(stops[1].position, Some(LengthPercentage::Percent(1.0)));
    match &stops[0].color {
        ColorValue::Absolute(c) => {
            approx(c.components[0], 1.0);
            approx(c.components[1], 0.0);
        }
        _ => panic!("首停点应为绝对红"),
    }
}

// ------------------------------------------------------------- object-fit 几何

const BOX: &str = "#root { width: 60px; height: 40px; }";

#[test]
fn object_fit_fill_stretches_full_box() {
    let mut e = engine_img(
        &format!("{BOX} #t {{ width: 60px; height: 40px; image: k; }}"),
        Some("k"),
        true,
    );
    // 根盒 (0,0,60,40)；叶拉伸同盒 → fill（默认）= 全盒。
    let (x, y, w, h) = image_rect(&mut e);
    approx(x, 0.0);
    approx(y, 0.0);
    approx(w, 60.0);
    approx(h, 40.0);
}

#[test]
fn object_fit_contain_letterboxes() {
    // s = min(60/40, 40/20) = 1.5 → 60×30，垂直居中 y=(40−30)×0.5=5。
    let mut e = engine_img(
        &format!("{BOX} #t {{ width: 60px; height: 40px; image: k; object-fit: contain; }}"),
        Some("k"),
        true,
    );
    let (x, y, w, h) = image_rect(&mut e);
    approx(x, 0.0);
    approx(y, 5.0);
    approx(w, 60.0);
    approx(h, 30.0);
}

#[test]
fn object_fit_cover_overflows_and_clips() {
    // s = max(1.5, 2) = 2 → 80×40；dx=(60−80)×0.5=−10 → 溢出 → PushClip 包裹。
    let mut e = engine_img(
        &format!("{BOX} #t {{ width: 60px; height: 40px; image: k; object-fit: cover; }}"),
        Some("k"),
        true,
    );
    let f = e.frame((800.0, 600.0), 1.0, 0.0);
    let has_clip = f
        .paint
        .ops
        .iter()
        .any(|op| matches!(op, PaintOp::PushClip { .. }));
    assert!(has_clip, "cover 应产生内容盒裁剪");
    let (x, y, w, h) = image_rect(&mut e);
    approx(x, -10.0);
    approx(y, 0.0);
    approx(w, 80.0);
    approx(h, 40.0);
}

#[test]
fn object_fit_none_keeps_natural_centered() {
    let mut e = engine_img(
        &format!("{BOX} #t {{ width: 60px; height: 40px; image: k; object-fit: none; }}"),
        Some("k"),
        true,
    );
    let (x, y, w, h) = image_rect(&mut e);
    approx(x, 10.0);
    approx(y, 10.0);
    approx(w, 40.0);
    approx(h, 20.0);
}

#[test]
fn object_fit_scale_down_takes_smaller() {
    // 盒 30×20：contain s=0.75 → 30×15；none=40×20 溢出；
    // scale-down = min(1, 0.75) = 0.75 → 与 contain 同。
    let mut e = engine_img(
        "#root { width: 30px; height: 20px; } #t { width: 30px; height: 20px; image: k; object-fit: scale-down; }",
        Some("k"),
        true,
    );
    let (x, y, w, h) = image_rect(&mut e);
    approx(x, 0.0);
    approx(y, 2.5);
    approx(w, 30.0);
    approx(h, 15.0);
}

#[test]
fn object_position_left_top_and_right_bottom() {
    // contain 60×30 于 60×40 盒：left top → (0,0)；right bottom → (0,10)。
    let mut a = engine_img(
        &format!(
            "{BOX} #t {{ width: 60px; height: 40px; image: k; object-fit: contain; object-position: left top; }}"
        ),
        Some("k"),
        true,
    );
    let (x, y, _, _) = image_rect(&mut a);
    approx(x, 0.0);
    approx(y, 0.0);

    let mut b = engine_img(
        &format!(
            "{BOX} #t {{ width: 60px; height: 40px; image: k; object-fit: contain; object-position: right bottom; }}"
        ),
        Some("k"),
        true,
    );
    let (x, y, _, _) = image_rect(&mut b);
    approx(x, 0.0);
    approx(y, 10.0);
}

#[test]
fn object_position_px_offsets() {
    // 20px 15px → 偏移 = px 值（盒−拟合=0 时百分比与 px 均为绝对位移）。
    let mut e = engine_img(
        &format!(
            "{BOX} #t {{ width: 60px; height: 40px; image: k; object-fit: none; object-position: 20px 15px; }}"
        ),
        Some("k"),
        true,
    );
    let (x, y, _, _) = image_rect(&mut e);
    approx(x, 20.0);
    approx(y, 15.0);
}

// --------------------------------------------------------- 替换内容叶布局

#[test]
fn image_leaf_sizes_to_natural_without_declaration() {
    // 无 width/height：固有注入（min=max=自然）→ 叶盒 = 40×20。
    let mut e = engine_img("#root { width: 400px; } #t { image: k; }", Some("k"), true);
    let (_, _, w, h) = leaf_box(&mut e);
    approx(w, 40.0);
    approx(h, 20.0);
}

#[test]
fn image_leaf_declared_width_scales_height() {
    // 声明宽 80px（2:1 源）→ 高 = 80/2 = 40（img{width:80px;height:auto}）。
    let mut e = engine_img(
        "#root { width: 400px; } #t { image: k; width: 80px; }",
        Some("k"),
        true,
    );
    let (_, _, w, h) = leaf_box(&mut e);
    approx(w, 80.0);
    approx(h, 40.0);
}

#[test]
fn unregistered_image_reference_skips() {
    // 未注册引用：无 Image op、无 panic（零副作用告警契约）。
    let mut e = engine_img(
        "#root { width: 60px; height: 40px; } #t { image: missing; }",
        Some("missing"),
        false,
    );
    let f = e.frame((800.0, 600.0), 1.0, 0.0);
    let none = f
        .paint
        .ops
        .iter()
        .all(|op| !matches!(op, PaintOp::Image { .. }));
    assert!(none, "未注册引用不应产生 Image op");
}

// ---------------------------------------------------------------- repeating 解析

/// repeating- 前缀三族（P1-3，css-images-3）：内层文法逐一相同，
/// 仅 repeating 标记不同；非 repeating 族仍为 false。
#[test]
fn repeating_gradient_flags_all_families() {
    for (css, kind_name) in [
        ("repeating-linear-gradient(45deg, red, blue)", "linear"),
        ("repeating-radial-gradient(circle, red, blue)", "radial"),
        ("repeating-conic-gradient(red, blue)", "conic"),
    ] {
        let mut e = engine_img(
            &format!("#t {{ width: 60px; height: 40px; background-image: {css}; }}"),
            None,
            false,
        );
        let _ = e.frame((800.0, 600.0), 1.0, 0.0);
        let style = e.computed_style(2).unwrap().clone();
        let Some(DeclValue::BackgroundImage(images)) = style.get(PropertyId::BackgroundImage)
        else {
            panic!("{css}: 背景图应为渐变");
        };
        let Some(BackgroundImage::Gradient(g)) = images.first() else {
            panic!("{css}: 背景图应为渐变");
        };
        assert!(g.repeating, "{css}: 应带 repeating 标记");
        match (&g.kind, kind_name) {
            (GradientKind::Linear(_), "linear")
            | (GradientKind::Radial(_), "radial")
            | (GradientKind::Conic(_), "conic") => {}
            _ => panic!("{css}: kind 应为 {kind_name}"),
        }
    }
    // 非 repeating：同文法无前缀 → false
    let mut e = engine_img(
        "#t { width: 60px; height: 40px; background-image: linear-gradient(red, blue); }",
        None,
        false,
    );
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    let style = e.computed_style(2).unwrap().clone();
    let Some(DeclValue::BackgroundImage(images)) = style.get(PropertyId::BackgroundImage) else {
        panic!("背景图应为渐变");
    };
    let Some(BackgroundImage::Gradient(g)) = images.first() else {
        panic!("背景图应为渐变");
    };
    assert!(!g.repeating);
}
