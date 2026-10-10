//! tiny sink 合成不变量质检（P10 沉淀）。
//!
//! 锁定三条 P10 实证约束：组缓冲 Pattern 锚定（identity 会把组内容
//! 位移 −(ox,oy)）、组遮罩 DestIn 合成、变换栈共轭次序。采样点全部
//! 避开 AA 边界行（路径边界整数 y 的像素行覆盖率为 0）。

use style_engine::{AlphaColor, DisplayList, PaintOp};
use style_engine_tiny::render;

fn px(list: &DisplayList) -> tiny_skia::Pixmap {
    render(list, 300, 200, [1.0, 1.0, 1.0, 1.0], 1.0)
}

fn is_red(pix: &tiny_skia::Pixmap, x: u32, y: u32) -> bool {
    let c = pix.pixel(x, y).unwrap();
    c.red() == 255 && c.green() == 0 && c.blue() == 0 && c.alpha() == 255
}

fn is_white(pix: &tiny_skia::Pixmap, x: u32, y: u32) -> bool {
    let c = pix.pixel(x, y).unwrap();
    c.red() == 255 && c.green() == 255 && c.blue() == 255 && c.alpha() == 255
}

fn red_op(x: f32, y: f32, w: f32, h: f32) -> PaintOp {
    PaintOp::FillRect {
        x,
        y,
        width: w,
        height: h,
        radius: [0.0; 8],
        color: AlphaColor::new([1.0, 0.0, 0.0, 1.0]),
    }
}

/// 同一 DisplayList 两次渲染逐字节一致（无外部状态、无并行竞态）。
#[test]
fn render_is_deterministic() {
    let ops = vec![
        PaintOp::PushClip {
            x: 10.0,
            y: 10.0,
            width: 50.0,
            height: 40.0,
            radius: [0.0; 8],
        },
        red_op(0.0, 0.0, 300.0, 200.0),
        PaintOp::PopClip,
    ];
    let list = DisplayList { generation: 0, ops };
    let a = px(&list);
    let b = px(&list);
    assert_eq!(a.data(), b.data(), "两次渲染输出不一致");
}

/// 回归锁定（transform-pixel 根因二）：变换栈内的裁剪组必须按设备
/// 坐标原位合成——rotate180 保 bbox affine 下内容落在原 bbox。
#[test]
fn clip_inside_transform_composites_at_device_position() {
    // rotate180 保 bbox：[-1,0,0,-1,200,120]（元素 (0,20,200,80)）。
    let rot = [-1.0f32, 0.0, 0.0, -1.0, 200.0, 120.0];
    let list = DisplayList {
        generation: 0,
        ops: vec![
            PaintOp::PushTransform { affine: rot },
            PaintOp::PushClip {
                x: 0.0,
                y: 20.0,
                width: 200.0,
                height: 80.0,
                radius: [0.0; 8],
            },
            red_op(0.0, 20.0, 200.0, 80.0),
            PaintOp::PopClip,
            PaintOp::PopTransform,
        ],
    };
    let pix = px(&list);
    assert!(is_red(&pix, 100, 60), "组内内容应红于 bbox 中心");
    assert!(is_white(&pix, 100, 110), "bbox 下方应保持底色（越界白）");
    assert!(is_white(&pix, 100, 10), "bbox 上方应保持底色");
}

/// 非整数 affine（rotate180 数值残差变体，P10 失败现场）同样原位。
#[test]
fn clip_inside_transform_fractional_affine_in_place() {
    let rot2 = [-1.0f32, -8.742278e-8, 8.742278e-8, -1.0, 119.999985, 300.0];
    let list = DisplayList {
        generation: 0,
        ops: vec![
            PaintOp::PushTransform { affine: rot2 },
            PaintOp::PushClip {
                x: 0.0,
                y: 120.0,
                width: 120.0,
                height: 60.0,
                radius: [0.0; 8],
            },
            red_op(0.0, 120.0, 120.0, 60.0),
            PaintOp::PopClip,
            PaintOp::PopTransform,
        ],
    };
    let pix = px(&list);
    assert!(is_red(&pix, 60, 130), "残差 affine 下组内容应红于 bbox 内");
    assert!(is_red(&pix, 60, 170), "bbox 内下缘亦应红（避开边界行）");
    assert!(is_white(&pix, 60, 110), "bbox 上方应保持底色");
}

/// 平移变换组：内容随 affine 平移，组外底色不变。
#[test]
fn translate_group_shifts_content() {
    let tr = [1.0f32, 0.0, 0.0, 1.0, 10.0, 0.0];
    let list = DisplayList {
        generation: 0,
        ops: vec![
            PaintOp::PushTransform { affine: tr },
            PaintOp::PushClip {
                x: 0.0,
                y: 20.0,
                width: 200.0,
                height: 80.0,
                radius: [0.0; 8],
            },
            red_op(0.0, 20.0, 200.0, 80.0),
            PaintOp::PopClip,
            PaintOp::PopTransform,
        ],
    };
    let pix = px(&list);
    assert!(is_red(&pix, 105, 60), "平移后 bbox 内应红");
    assert!(is_white(&pix, 5, 60), "bbox 左侧应保持底色");
}

/// 透明度组：半透明内容与底色混合，位置不漂移。
#[test]
fn opacity_group_blends_in_place() {
    let ops = vec![
        PaintOp::PushOpacity {
            alpha: 0.5,
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        },
        red_op(0.0, 0.0, 100.0, 100.0),
        PaintOp::PopOpacity,
    ];
    let pix = px(&DisplayList { generation: 0, ops });
    let c = pix.pixel(50, 50).unwrap();
    // 白底上 0.5 红：(255, 127.5→128, 127.5→128, 255)。
    assert_eq!(
        (c.red(), c.green(), c.blue(), c.alpha()),
        (255, 128, 128, 255)
    );
}

/// world 缩放：scale=2 时逻辑坐标翻倍落设备。
#[test]
fn render_scale_doubles_geometry() {
    let list = DisplayList {
        generation: 0,
        ops: vec![red_op(10.0, 10.0, 20.0, 20.0)],
    };
    let pix = render(&list, 120, 120, [1.0, 1.0, 1.0, 1.0], 2.0);
    assert!(
        is_red(&pix, 30, 30),
        "scale=2 下逻辑 (15,15) 应映射设备 (30,30)"
    );
    assert!(is_white(&pix, 15, 15), "scale=2 下设备 (15,15) 应保持底色");
}

/// 空列表仅铺底色。
#[test]
fn empty_list_is_base_color() {
    let pix = px(&DisplayList {
        generation: 0,
        ops: vec![],
    });
    assert_eq!(pix.data().iter().filter(|&&b| b != 255).count(), 0);
}
