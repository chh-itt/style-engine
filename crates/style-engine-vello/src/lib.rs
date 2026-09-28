//! `style-engine-vello` — style-engine DisplayList 的 Vello (wgpu) 绘制后端。
//!
//! ADR-0002：颜色为 sRGB 直传（vello 内部按线性混合，颜色空间语义由
//! core 的 DisplayList 契约承接）；滚动偏移折叠为坐标平移；裁剪走
//! vello 图层（Mix::Clip）。
//!
//! MVP 偏差（FEATURES.md 同步）：
//! - Shadow 无模糊（vello 0.10 无内置高斯模糊），以半透明矩形近似；
//! - Text 基元跳过（字形 run 随 T5 落地后接入 vello 的 parley 绘制）。

use style_engine::css::value::ColorValue;
use style_engine::{DisplayList, PaintOp};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, RoundedRectRadii, Stroke, Vec2};
use vello::peniko::color::{AlphaColor, Srgb};
use vello::peniko::{Brush, Extend, Fill, Gradient, GradientKind, Mix, RadialGradientPosition};

/// 将绘制清单写入 vello 场景（追加语义；调用方持有场景生命周期）。
pub fn render_ops(list: &DisplayList, scene: &mut Scene) {
    let mut state = RenderState::default();
    for op in &list.ops {
        apply_op(op, scene, &mut state);
    }
}

/// 构建独立场景。
pub fn render(list: &DisplayList) -> Scene {
    let mut scene = Scene::new();
    render_ops(list, &mut scene);
    scene
}

#[derive(Default)]
struct RenderState {
    /// 累计滚动平移（PushScroll/PopScroll 栈）。
    offset: Vec2,
    stack: Vec<Vec2>,
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, radius: [f32; 4]) -> RoundedRect {
    let rect = Rect::new(
        f64::from(x),
        f64::from(y),
        f64::from(x + w),
        f64::from(y + h),
    );
    // kurbo 顺序：左上、右上、右下、左下（与 DisplayList 约定一致）
    let radii = RoundedRectRadii::new(
        f64::from(radius[0]),
        f64::from(radius[1]),
        f64::from(radius[2]),
        f64::from(radius[3]),
    );
    RoundedRect::from_rect(rect, radii)
}

fn rect_shape(x: f32, y: f32, w: f32, h: f32, radius: [f32; 4]) -> RoundedRect {
    rounded_rect(x, y, w, h, radius)
}

fn apply_op(op: &PaintOp, scene: &mut Scene, state: &mut RenderState) {
    match op {
        PaintOp::FillRect {
            x,
            y,
            width,
            height,
            radius,
            color,
        } => {
            let shape = rect_shape(
                *x + state.offset.x as f32,
                *y + state.offset.y as f32,
                *width,
                *height,
                *radius,
            );
            scene.fill(Fill::NonZero, Affine::IDENTITY, *color, None, &shape);
        }
        PaintOp::Gradient {
            x,
            y,
            width,
            height,
            radius,
            gradient,
        } => {
            let shape = rect_shape(
                *x + state.offset.x as f32,
                *y + state.offset.y as f32,
                *width,
                *height,
                *radius,
            );
            let brush = peniko_gradient(gradient, *width, *height, state);
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Brush::Gradient(&brush),
                None,
                &shape,
            );
        }
        PaintOp::Shadow {
            x,
            y,
            width,
            height,
            radius,
            color,
            offset_x,
            offset_y,
            ..
        } => {
            // 偏差：无模糊，半透明矩形近似
            let shape = rect_shape(
                *x + state.offset.x as f32 + offset_x,
                *y + state.offset.y as f32 + offset_y,
                *width,
                *height,
                *radius,
            );
            scene.fill(Fill::NonZero, Affine::IDENTITY, *color, None, &shape);
        }
        PaintOp::Border {
            x,
            y,
            width,
            height,
            radius,
            color,
            style,
            border_width,
        } => {
            let shape = rect_shape(
                *x + state.offset.x as f32,
                *y + state.offset.y as f32,
                *width,
                *height,
                *radius,
            );
            let mut stroke = Stroke::new(f64::from(*border_width));
            match style {
                style_engine::css::property::BorderStyle::Dashed => {
                    stroke = stroke.with_dashes(0.0, [f64::from(*border_width) * 3.0]);
                }
                style_engine::css::property::BorderStyle::Dotted => {
                    stroke = stroke
                        .with_caps(vello::kurbo::Cap::Round)
                        .with_dashes(0.0, [0.0, f64::from(*border_width) * 2.0]);
                }
                _ => {}
            }
            scene.stroke(&stroke, Affine::IDENTITY, *color, None, &shape);
        }
        PaintOp::Text { .. } => {
            // 偏差：字形 run 随 T5 落地
        }
        PaintOp::PushClip {
            x,
            y,
            width,
            height,
            radius,
        } => {
            let shape = rect_shape(
                *x + state.offset.x as f32,
                *y + state.offset.y as f32,
                *width,
                *height,
                *radius,
            );
            // 形状本身完成裁剪；混合取 Normal（vello 0.10 无 Mix::Clip）
            scene.push_layer(Fill::NonZero, Mix::Normal, 1.0, Affine::IDENTITY, &shape);
        }
        PaintOp::PopClip => {
            scene.pop_layer();
        }
        PaintOp::PushScroll { dx, dy } => {
            state.stack.push(state.offset);
            state.offset += Vec2::new(f64::from(*dx), f64::from(*dy));
        }
        PaintOp::PopScroll => {
            state.offset = state.stack.pop().unwrap_or(Vec2::ZERO);
        }
        // PaintOp #[non_exhaustive]：后续基元先忽略
        _ => {}
    }
}

/// 引擎 Gradient → peniko Gradient（CSS 渐变线几何；stop 缺省位置均匀分配）。
fn peniko_gradient(
    g: &style_engine::css::property::Gradient,
    w: f32,
    h: f32,
    state: &RenderState,
) -> Gradient {
    let mut out = match &g.kind {
        style_engine::css::property::GradientKind::Linear(angle) => {
            // CSS 角度：0 = 向上，顺时针；方向向量
            let rad = angle.0.to_radians();
            let (sin, cos) = rad.sin_cos();
            let dir = Vec2::new(sin as f64, -(cos as f64));
            // 渐变线长度：|W·sinθ| + |H·cosθ|
            let line = ((w * sin.abs()) + (h * cos.abs())) as f64 / 2.0;
            let cx = f64::from(w) / 2.0 + state.offset.x;
            let cy = f64::from(h) / 2.0 + state.offset.y;
            let start = Point::new(cx - dir.x * line, cy - dir.y * line);
            let end = Point::new(cx + dir.x * line, cy + dir.y * line);
            Gradient::new_linear(start, end)
        }
        style_engine::css::property::GradientKind::Radial => {
            // MVP：正圆、中心点、半径 = 到最远角（farthest-corner 近似对角线/2）
            Gradient {
                kind: GradientKind::Radial(RadialGradientPosition::new(
                    Point::new(
                        f64::from(w) / 2.0 + state.offset.x,
                        f64::from(h) / 2.0 + state.offset.y,
                    ),
                    (((f64::from(w) / 2.0).powi(2) + (f64::from(h) / 2.0).powi(2)).sqrt()) as f32,
                )),
                ..Default::default()
            }
        }
    };
    out.extend = Extend::Pad;
    for (p, c) in distribute_stops(&g.stops) {
        out.stops.push(vello::peniko::ColorStop {
            offset: p,
            color: c.into(),
        });
    }
    out
}

/// 按补齐 CSS 语义的 stop 位置构建 (offset, sRGB 颜色) 序列。
fn distribute_stops(
    stops: &[style_engine::css::property::ColorStop],
) -> Vec<(f32, AlphaColor<Srgb>)> {
    let n = stops.len();
    if n == 0 {
        return Vec::new();
    }
    let mut positions: Vec<f32> = Vec::with_capacity(n);
    for s in stops {
        match &s.position {
            Some(p) => positions.push(
                p.resolve(
                    &style_engine::css::value::ResolveCtx {
                        em: 16.0,
                        rem: 16.0,
                        viewport_w: 0.0,
                        viewport_h: 0.0,
                    },
                    0.0,
                )
                .unwrap_or(0.0),
            ),
            None => positions.push(f32::NAN),
        }
    }
    if positions[0].is_nan() {
        positions[0] = 0.0;
    }
    if positions[n - 1].is_nan() {
        positions[n - 1] = 1.0;
    }
    let mut last_known = 0.0f32;
    let mut i = 0;
    while i < n {
        if positions[i].is_nan() {
            let mut j = i;
            while j < n && positions[j].is_nan() {
                j += 1;
            }
            let next = if j < n { positions[j] } else { 1.0 };
            let span = (j - i + 1) as f32;
            for (k, idx) in (i..j).enumerate() {
                positions[idx] = last_known + (next - last_known) * ((k + 1) as f32 / span);
            }
            i = j;
        } else {
            last_known = positions[i];
            i += 1;
        }
    }
    stops
        .iter()
        .zip(positions)
        .map(|(s, p)| {
            let c = match s.color.pick_scheme(false) {
                ColorValue::Absolute(c) => c,
                _ => AlphaColor::new([0.0, 0.0, 0.0, 1.0]),
            };
            (p, c)
        })
        .collect()
}
