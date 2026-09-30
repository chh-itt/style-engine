//! `style-engine-soft` — DisplayList 的纯软件绘制后端（第二 Sink，第五批㉛）。
//!
//! 职责：验证 DisplayList 契约的 **sink 无关性**——零 GPU、零第三方依赖
//! （纯 Rust 标准库软件光栅化），像素确定性可作 CI 级绘制断言的参照实现。
//!
//! 合成语义（与 ㉕ 探针实测对齐）：sRGB 编码值直接 src-over（=vello 0.10
//! Rgba8Unorm 路径与 Chromium/CSS 默认一致）；渐变停点 sRGB 插值
//! （CSS 默认插值空间）。
//!
//! v0 覆盖矩阵：
//!
//! | op | 支持 | 备注 |
//! |---|---|---|
//! | FillRect | ✓ | 椭圆圆角逐像素覆盖测试 |
//! | Gradient | ✓ | linear（CSS 角度）+ radial（RadialGeom 椭圆）；停点色仅 Absolute（其余视作全透明），位置仅 Px/Percent（其余/None 自动均布） |
//! | Shadow | 近似 | 平移半透明矩形（blur 忽略——与 vello sink MVP 同偏差；inset=与盒求交） |
//! | Image | ✓ | 最近邻采样（缩放无滤波） |
//! | Border | 近似 | 直边带（Solid；Dashed/Dotted 近似为实线；圆角未斜切） |
//! | PushClip/PopClip | ✓ | 矩形+圆角裁剪栈 |
//! | PushOpacity/PopOpacity | ✓ | 有界组 alpha（快照回混，ADR-0008） |
//! | PushScroll/PopScroll | ✓ | 平移折叠（嵌套累加） |
//! | PushTransform/PopTransform | ✗ v0 跳过 | 变换层不消费（记录） |
//! | Text | ✗ v0 跳过 | 需 shaping——与 vello sink MVP 同注 |

use style_engine::css::property::{BorderStyle, ColorStop, GradientKind};
use style_engine::css::value::{ColorValue, LengthPercentage};
use style_engine::paint::{ImageRes, RadialGeom};
use style_engine::{DisplayList, PaintOp};

/// 纯软件画布：RGBA8 直 alpha、sRGB 编码值（与 DisplayList 色彩语义一致）。
pub struct SoftCanvas {
    pub width: u32,
    pub height: u32,
    /// 行主序 RGBA8，`pixels.len() == width * height * 4`。
    pub pixels: Vec<u8>,
}

struct ClipRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: [f32; 8],
}

struct OpacityLayer {
    snapshot: Vec<u8>,
    alpha: f32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// 将绘制清单光栅化到 `width×height` 画布（`base` 为初始底色）。
pub fn render(list: &DisplayList, width: u32, height: u32, base: [u8; 4]) -> SoftCanvas {
    let mut canvas = SoftCanvas {
        width,
        height,
        pixels: vec![0u8; width as usize * height as usize * 4],
    };
    for px in canvas.pixels.chunks_exact_mut(4) {
        px.copy_from_slice(&base);
    }
    let mut clips: Vec<ClipRect> = Vec::new();
    let mut scroll: (f32, f32) = (0.0, 0.0);
    let mut scroll_stack: Vec<(f32, f32)> = Vec::new();
    let mut layers: Vec<OpacityLayer> = Vec::new();
    for op in &list.ops {
        apply_op(
            op,
            &mut canvas,
            &mut clips,
            &mut scroll,
            &mut scroll_stack,
            &mut layers,
        );
    }
    canvas
}

fn apply_op(
    op: &PaintOp,
    canvas: &mut SoftCanvas,
    clips: &mut Vec<ClipRect>,
    scroll: &mut (f32, f32),
    scroll_stack: &mut Vec<(f32, f32)>,
    layers: &mut Vec<OpacityLayer>,
) {
    let (dx, dy) = *scroll;
    match op {
        PaintOp::FillRect {
            x,
            y,
            width,
            height,
            radius,
            color,
        } => {
            let c = components_of_color(color.components);
            fill_rect(
                canvas,
                clips,
                x + dx,
                y + dy,
                *width,
                *height,
                radius,
                move |_, _| c,
            );
        }
        PaintOp::Gradient {
            x,
            y,
            width,
            height,
            radius,
            gradient,
            radial,
        } => {
            let (geom, line_len) = match gradient.kind {
                GradientKind::Linear(angle) => {
                    // CSS 角度：0deg=向上，顺时针；方向向量 = (sin θ, -cos θ)
                    let rad = angle.0.to_radians();
                    let (sx, sy) = (rad.sin(), -rad.cos());
                    let len = (width * sx).abs() + (height * sy).abs();
                    (None, (sx, sy, len.max(1e-6)))
                }
                GradientKind::Radial(_) => {
                    // paint 层已解析绝对几何（RadialGeom）；渐变线 = 水平半径
                    let g = radial.unwrap_or(RadialGeom {
                        cx: x + width / 2.0,
                        cy: y + height / 2.0,
                        rx: width.max(1.0) / 2.0,
                        ry: height.max(1.0) / 2.0,
                    });
                    (Some(g), (0.0, 0.0, g.rx.max(1e-6)))
                }
            };
            let (bx, by) = (x + dx, y + dy);
            let stops = gradient.stops.clone();
            fill_rect(
                canvas,
                clips,
                bx,
                by,
                *width,
                *height,
                radius,
                move |fx, fy| {
                    let t = match geom {
                        // 线性：中心点投影归一（线过盒中心，长 = |w·sin|+|h·cos|）
                        None => {
                            let (sx, sy, len) = line_len;
                            let cx = bx + width / 2.0;
                            let cy = by + height / 2.0;
                            ((fx - cx) * sx + (fy - cy) * sy) / len + 0.5
                        }
                        // 径向：椭圆归一距离
                        Some(g) => {
                            let nx = (fx - (g.cx + dx)) / g.rx;
                            let ny = (fy - (g.cy + dy)) / g.ry;
                            (nx * nx + ny * ny).sqrt()
                        }
                    };
                    stop_at(&stops, t.clamp(0.0, 1.0), line_len.2)
                },
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
            spread,
            inset,
            ..
        } => {
            // 模糊忽略（与 vello sink MVP 同偏差）：外阴影=外扩平移矩形；
            // 内阴影=平移矩形与盒体求交。
            let (sx, sy, sw, sh) = if *inset {
                let (ix0, iy0) = ((x + offset_x).max(*x), (y + offset_y).max(*y));
                let (ix1, iy1) = (
                    (x + offset_x + width + spread).min(x + width),
                    (y + offset_y + height + spread).min(y + height),
                );
                (ix0, iy0, (ix1 - ix0).max(0.0), (iy1 - iy0).max(0.0))
            } else {
                (
                    x + offset_x - spread,
                    y + offset_y - spread,
                    width + 2.0 * spread,
                    height + 2.0 * spread,
                )
            };
            let c = components_of_color(color.components);
            fill_rect(
                canvas,
                clips,
                sx + dx,
                sy + dy,
                sw,
                sh,
                radius,
                move |_, _| c,
            );
        }
        PaintOp::Image {
            x,
            y,
            width,
            height,
            radius,
            source_w,
            source_h,
            pixels,
        } => {
            draw_image(
                canvas,
                clips,
                x + dx,
                y + dy,
                *width,
                *height,
                radius,
                pixels,
            );
            let _ = (source_w, source_h);
        }
        PaintOp::Border {
            x,
            y,
            width,
            height,
            radius,
            sides,
        } => {
            // 直边带（Solid）；Dashed/Dotted 近似实线；圆角未斜切（记录）。
            let bands = [
                (*x, *y, *width, sides[0].width),                            // top
                (*x + *width - sides[1].width, *y, sides[1].width, *height), // right
                (*x, *y + *height - sides[3].width, *width, sides[3].width), // bottom
                (*x, *y, sides[2].width, *height),                           // left
            ];
            let _ = radius;
            for (i, side) in sides.iter().enumerate() {
                if side.width <= 0.0 || side.style == BorderStyle::None {
                    continue;
                }
                let comp = components_of_color(side.color.components);
                let (bx, by, bw, bh) = bands[i];
                fill_rect(
                    canvas,
                    clips,
                    bx + dx,
                    by + dy,
                    bw,
                    bh,
                    &[0.0; 8],
                    move |_, _| comp,
                );
            }
        }
        PaintOp::PushClip {
            x,
            y,
            width,
            height,
            radius,
        } => clips.push(ClipRect {
            x: x + dx,
            y: y + dy,
            w: *width,
            h: *height,
            radius: *radius,
        }),
        PaintOp::PopClip => {
            clips.pop();
        }
        PaintOp::PushOpacity {
            alpha,
            x,
            y,
            width,
            height,
        } => {
            layers.push(OpacityLayer {
                snapshot: canvas.pixels.clone(),
                alpha: alpha.clamp(0.0, 1.0),
                x: (x + dx).max(0.0) as u32,
                y: (y + dy).max(0.0) as u32,
                w: (*width).max(0.0) as u32,
                h: (*height).max(0.0) as u32,
            });
        }
        PaintOp::PopOpacity => {
            if let Some(layer) = layers.pop() {
                let x1 = (layer.x + layer.w).min(canvas.width);
                let y1 = (layer.y + layer.h).min(canvas.height);
                for py in layer.y..y1 {
                    for px in layer.x..x1 {
                        let i = (py as usize * canvas.width as usize + px as usize) * 4;
                        for c in 0..4 {
                            let cur = canvas.pixels[i + c] as f32;
                            let snap = layer.snapshot[i + c] as f32;
                            canvas.pixels[i + c] = (snap + layer.alpha * (cur - snap))
                                .round()
                                .clamp(0.0, 255.0)
                                as u8;
                        }
                    }
                }
            }
        }
        PaintOp::PushScroll { dx: ndx, dy: ndy } => {
            scroll_stack.push(*scroll);
            scroll.0 += ndx;
            scroll.1 += ndy;
        }
        PaintOp::PopScroll => {
            if let Some(prev) = scroll_stack.pop() {
                *scroll = prev;
            }
        }
        // v0 跳过：变换层与文本（覆盖矩阵见模块注）；未识别 op 一并忽略
        //（PaintOp 非穷举演进——契约 sink 对未来 op 的默认语义=忽略）。
        _ => {}
    }
}

/// AlphaColor 分量 → (r, g, b, a) f32 元组。
fn components_of_color(c: [f32; 4]) -> [f32; 4] {
    c
}

/// 椭圆圆角覆盖测试：像素位于某角方内时按归一椭圆距离判定。
fn corner_ok(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: &[f32; 8]) -> bool {
    let corners = [
        (
            r[0],
            r[1],
            x + r[0],
            y + r[1],
            px < x + r[0] && py < y + r[1],
        ), // tl
        (
            r[2],
            r[3],
            x + w - r[2],
            y + r[3],
            px >= x + w - r[2] && py < y + r[3],
        ), // tr
        (
            r[4],
            r[5],
            x + w - r[4],
            y + h - r[5],
            px >= x + w - r[4] && py >= y + h - r[5],
        ), // br
        (
            r[6],
            r[7],
            x + r[6],
            y + h - r[7],
            px < x + r[6] && py >= y + h - r[7],
        ), // bl
    ];
    for (rx, ry, cx, cy, in_square) in corners {
        if in_square && rx > 0.0 && ry > 0.0 {
            let nx = (px - cx) / rx;
            let ny = (py - cy) / ry;
            return nx * nx + ny * ny <= 1.0;
        }
    }
    true
}

fn clip_ok(px: f32, py: f32, c: &ClipRect) -> bool {
    if px < c.x || px >= c.x + c.w || py < c.y || py >= c.y + c.h {
        return false;
    }
    corner_ok(px, py, c.x, c.y, c.w, c.h, &c.radius)
}

/// sRGB 编码值直接 src-over（㉕ 实测语义；alpha 直 alpha 合成）。
fn blend(dst: &mut [u8], src: [f32; 4]) {
    let a = src[3].clamp(0.0, 1.0);
    for i in 0..3 {
        dst[i] = (src[i].clamp(0.0, 1.0) * a * 255.0 + dst[i] as f32 * (1.0 - a))
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    let da = dst[3] as f32 / 255.0;
    dst[3] = ((a + da * (1.0 - a)) * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// 矩形区域逐像素填充（像素光栅器签名天然多参——圆角/裁剪/取色闭包）。
#[allow(clippy::too_many_arguments)]
fn fill_rect(
    canvas: &mut SoftCanvas,
    clips: &[ClipRect],
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: &[f32; 8],
    color_at: impl Fn(f32, f32) -> [f32; 4],
) {
    let x0 = x.floor().max(0.0) as i64;
    let y0 = y.floor().max(0.0) as i64;
    let x1 = ((x + w).ceil() as i64).min(canvas.width as i64);
    let y1 = ((y + h).ceil() as i64).min(canvas.height as i64);
    for py in y0..y1 {
        for px in x0..x1 {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            if fx < x || fx >= x + w || fy < y || fy >= y + h {
                continue;
            }
            if !corner_ok(fx, fy, x, y, w, h, radius) {
                continue;
            }
            if !clips.iter().all(|c| clip_ok(fx, fy, c)) {
                continue;
            }
            let c = color_at(fx, fy);
            let i = (py as usize * canvas.width as usize + px as usize) * 4;
            blend(&mut canvas.pixels[i..i + 4], c);
        }
    }
}

/// 渐变停点：位置 Px（沿渐变线 px）/Percent（线长分数）直接解析，
/// 其余与 None 自动均布（CSS 简化规则：首 0 末 1，中间取邻点中点）。
fn stop_positions(stops: &[ColorStop], line_len: f32) -> Vec<f32> {
    let n = stops.len();
    let mut pos: Vec<Option<f32>> = stops
        .iter()
        .map(|s| match &s.position {
            Some(LengthPercentage::Px(v)) => Some((v / line_len).clamp(0.0, 1.0)),
            Some(LengthPercentage::Percent(f)) => Some(f.clamp(0.0, 1.0)),
            _ => None,
        })
        .collect();
    if n > 0 && pos[0].is_none() {
        pos[0] = Some(0.0);
    }
    if n > 1 && pos[n - 1].is_none() {
        pos[n - 1] = Some(1.0);
    }
    // 简化均布：从左到右，未定 = 前一已定值；再从右到左补全为 1 起点。
    for i in 1..n {
        if pos[i].is_none() {
            pos[i] = pos[i - 1];
        }
    }
    for i in (0..n.saturating_sub(1)).rev() {
        if pos[i].is_none() {
            pos[i] = pos[i + 1];
        }
    }
    pos.into_iter().map(|p| p.unwrap_or(0.0)).collect()
}

/// t 处停点色（sRGB 插值；非 Absolute 停点色视作全透明——记录偏差）。
fn stop_at(stops: &[ColorStop], t: f32, line_len: f32) -> [f32; 4] {
    if stops.is_empty() {
        return [0.0, 0.0, 0.0, 0.0];
    }
    let pos = stop_positions(stops, line_len);
    let rgba = |c: &ColorValue| match c {
        ColorValue::Absolute(a) => a.components,
        _ => [0.0, 0.0, 0.0, 0.0],
    };
    if t <= pos[0] {
        return rgba(&stops[0].color);
    }
    for i in 1..stops.len() {
        if t <= pos[i] {
            let (p0, p1) = (pos[i - 1], pos[i]);
            let k = if p1 > p0 { (t - p0) / (p1 - p0) } else { 0.0 };
            let (a, b) = (rgba(&stops[i - 1].color), rgba(&stops[i].color));
            return [
                a[0] + (b[0] - a[0]) * k,
                a[1] + (b[1] - a[1]) * k,
                a[2] + (b[2] - a[2]) * k,
                a[3] + (b[3] - a[3]) * k,
            ];
        }
    }
    rgba(&stops[stops.len() - 1].color)
}

/// 背景图最近邻采样（无滤波缩放；圆角覆盖与 FillRect 同法）。
#[allow(clippy::too_many_arguments)]
fn draw_image(
    canvas: &mut SoftCanvas,
    clips: &[ClipRect],
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: &[f32; 8],
    img: &ImageRes,
) {
    if img.width == 0 || img.height == 0 {
        return;
    }
    let bytes = img.rgba.as_ref().as_ref();
    let x0 = x.floor().max(0.0) as i64;
    let y0 = y.floor().max(0.0) as i64;
    let x1 = ((x + w).ceil() as i64).min(canvas.width as i64);
    let y1 = ((y + h).ceil() as i64).min(canvas.height as i64);
    for py in y0..y1 {
        for px in x0..x1 {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            if fx < x || fx >= x + w || fy < y || fy >= y + h {
                continue;
            }
            if !corner_ok(fx, fy, x, y, w, h, radius) {
                continue;
            }
            if !clips.iter().all(|c| clip_ok(fx, fy, c)) {
                continue;
            }
            let sx = (((fx - x) / w) * img.width as f32) as u32;
            let sy = (((fy - y) / h) * img.height as f32) as u32;
            let sx = sx.min(img.width - 1);
            let sy = sy.min(img.height - 1);
            let si = (sy as usize * img.width as usize + sx as usize) * 4;
            let i = (py as usize * canvas.width as usize + px as usize) * 4;
            blend(
                &mut canvas.pixels[i..i + 4],
                [
                    bytes[si] as f32 / 255.0,
                    bytes[si + 1] as f32 / 255.0,
                    bytes[si + 2] as f32 / 255.0,
                    bytes[si + 3] as f32 / 255.0,
                ],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peniko::color::{AlphaColor, Srgb};
    use style_engine::css::property::Gradient;
    use style_engine::css::value::Angle;
    use style_engine::paint::DisplayList;

    /// 测试辅助：f32 分量 → PaintOp 所用的 AlphaColor<Srgb>。
    fn rgba(color: [f32; 4]) -> AlphaColor<Srgb> {
        AlphaColor::new(color)
    }

    fn op_fill(x: f32, y: f32, w: f32, h: f32, r: [f32; 8], color: [f32; 4]) -> PaintOp {
        PaintOp::FillRect {
            x,
            y,
            width: w,
            height: h,
            radius: r,
            color: rgba(color),
        }
    }

    #[test]
    fn solid_fill_full_coverage() {
        let mut list = DisplayList::default();
        list.ops
            .push(op_fill(0.0, 0.0, 8.0, 8.0, [0.0; 8], [1.0, 0.0, 0.0, 1.0]));
        let c = render(&list, 8, 8, [255, 255, 255, 255]);
        assert_eq!(&c.pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&c.pixels[28..32], &[255, 0, 0, 255]);
    }

    #[test]
    fn rounded_corner_mask() {
        let mut list = DisplayList::default();
        list.ops
            .push(op_fill(0.0, 0.0, 8.0, 8.0, [4.0; 8], [0.0, 0.0, 1.0, 1.0]));
        let c = render(&list, 8, 8, [255, 255, 255, 255]);
        // (0,0) 在 tl 角方内、椭圆外 → 底色白
        assert_eq!(&c.pixels[0..4], &[255, 255, 255, 255]);
        // (4,4) 不在任何角方 → 蓝色
        let i = (4 * 8 + 4) * 4;
        assert_eq!(&c.pixels[i..i + 4], &[0, 0, 255, 255]);
    }

    #[test]
    fn translucent_blend_matches_vello_probe() {
        // ㉕ 交叉验证：红底 + 50% 白罩 → G=128（vello 实测同值，sRGB 合成）
        let mut list = DisplayList::default();
        list.ops
            .push(op_fill(0.0, 0.0, 8.0, 8.0, [0.0; 8], [1.0, 0.0, 0.0, 1.0]));
        list.ops
            .push(op_fill(0.0, 0.0, 8.0, 8.0, [0.0; 8], [1.0, 1.0, 1.0, 0.5]));
        let c = render(&list, 8, 8, [0, 0, 0, 255]);
        assert_eq!(&c.pixels[0..4], &[255, 128, 128, 255]);
    }

    #[test]
    fn linear_gradient_endpoints() {
        // 90deg（向右）黑→白：左缘黑、右缘近白
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::Gradient {
            x: 0.0,
            y: 0.0,
            width: 8.0,
            height: 2.0,
            radius: [0.0; 8],
            gradient: Gradient {
                kind: GradientKind::Linear(Angle(90.0)),
                stops: vec![
                    ColorStop {
                        color: ColorValue::Absolute(rgba([0.0, 0.0, 0.0, 1.0])),
                        position: Some(LengthPercentage::Px(0.0)),
                    },
                    ColorStop {
                        color: ColorValue::Absolute(rgba([1.0, 1.0, 1.0, 1.0])),
                        position: Some(LengthPercentage::Px(8.0)),
                    },
                ],
            },
            radial: None,
        });
        let c = render(&list, 8, 2, [0, 0, 0, 255]);
        // 像素中心采样：左缘 t=0.5/8 → 255×0.0625=15.9→16；右缘 t=7.5/8 → 239
        assert_eq!(&c.pixels[0..3], &[16, 16, 16], "左缘应为 t=1/16 灰");
        let right = 7 * 4; // (行 0, 列 7)
        assert_eq!(
            &c.pixels[right..right + 3],
            &[239, 239, 239],
            "右缘应为 t=15/16 灰"
        );
    }

    #[test]
    fn clip_bounds() {
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::PushClip {
            x: 0.0,
            y: 0.0,
            width: 4.0,
            height: 8.0,
            radius: [0.0; 8],
        });
        list.ops
            .push(op_fill(0.0, 0.0, 8.0, 8.0, [0.0; 8], [1.0, 0.0, 0.0, 1.0]));
        list.ops.push(PaintOp::PopClip);
        let c = render(&list, 8, 8, [255, 255, 255, 255]);
        assert_eq!(&c.pixels[0..4], &[255, 0, 0, 255]);
        let right = 6 * 4; // (行 0, 列 6)
        assert_eq!(&c.pixels[right..right + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn opacity_group_alpha() {
        // ADR-0008 组 alpha：快照白 + 50% alpha 子树红 → (255,128,128)
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::PushOpacity {
            alpha: 0.5,
            x: 0.0,
            y: 0.0,
            width: 8.0,
            height: 8.0,
        });
        list.ops
            .push(op_fill(0.0, 0.0, 8.0, 8.0, [0.0; 8], [1.0, 0.0, 0.0, 1.0]));
        list.ops.push(PaintOp::PopOpacity);
        let c = render(&list, 8, 8, [255, 255, 255, 255]);
        assert_eq!(&c.pixels[0..4], &[255, 128, 128, 255]);
    }
}
