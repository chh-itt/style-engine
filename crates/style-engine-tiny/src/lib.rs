//! `style-engine-tiny` — a pure-CPU tiny-skia drawing backend for the
//! style-engine DisplayList.
//!
//! Positioning (ADR-0043): the third drawing sink — the tiny-skia 0.12
//! rasterizer (the one resvg uses) + skrifa 0.44 glyph outlines. It shares
//! the filter/blur/compositing infrastructure with
//! [`style-engine-soft`](style_engine_soft) (single source of truth:
//! `filter::apply_effects`, `blur_alpha_u8`, `blend_pixel`), and shares the
//! parley typesetting driver plus the border/decoration geometry blueprints
//! with the vello sink (port sources annotated at each site).
//!
//! Capability differences relative to the other two sinks (kept in sync with
//! SINK-MATRIX):
//! - Shadow/Filter/BackdropFilter/Text shadows all use a **true shape mask +
//!   three-pass box blur** (same-source `blur_alpha_u8` as soft) — vello's
//!   concentric-ring approximation is upgraded everywhere in tiny;
//! - Blend modes: the 16 standard modes go through tiny-skia's native
//!   pipeline, `PlusLighter` maps to tiny `BlendMode::Plus`, and `PlusDarker`
//!   (missing from tiny-skia) is hand-written per pixel via soft's
//!   `blend_pixel` — 18/18 full coverage;
//! - Conic gradient start-angle normalization (`rem_euclid` + `Extend::Repeat`
//!   sampling of the fract) — the same mod semantics as Chrome/soft (vello's
//!   Pad passthrough renders the top-right quadrant wrong when from≠90°;
//!   SINK-MATRIX tracks the vello-side upgrade as pending);
//! - Radial gradient ellipse correction: placement =
//!   translate(c)∘scale(rx,ry)∘translate(−c) with a unit-radius brush and
//!   sampling through the inverse — the math direction is opposite to
//!   vello's brush scaling (vello's rx/ry appear reversed; no golden
//!   coverage; open question in SINK-MATRIX).
//!
//! Model: Push\* opens a **group buffer** (a device-pixel AABB sub-canvas +
//! optional mask), Pop\* composites back onto the parent target
//! (`fill_rect` + Pattern, or per-pixel for PlusDarker);
//! PushScroll/PushTransform are numeric stacks (isomorphic to vello,
//! including the transform×scroll conjugate `effective()` with its Tier C
//! approximation contract); DisplayList coordinates are logical px and
//! `scale` is folded into every drawing transform (`world`) — the same
//! semantics as the scene-level scaling in vello `render_offscreen`.

use std::mem;
use std::sync::Arc;

use style_engine::css::property::{
    BlendMode, FamilyName, FontFamilyList, Gradient, GradientKind, OverflowWrapKind, TextAlign,
    TextDecoStyleKind, WordBreakKind,
};
use style_engine::css::value::LengthPercentage;
use style_engine::paint::{ConicGeom, FilterEffect, LinearGeom, RadialGeom};
use style_engine::{AlphaColor, DisplayList, PaintOp, Srgb};
use style_engine_soft::filter::{apply_effects, effects_pad};
use style_engine_soft::{blend_pixel, blur_alpha_u8};
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, IntSize, LinearGradient, Paint, Path,
    PathBuilder, Pattern, Pixmap, Point, RadialGradient, Shader, SpreadMode, SweepGradient,
    Transform,
};

/// Logical px → device px whole-level scale (the `scale` parameter of
/// `render*`).
///
/// DisplayList coordinates are logical px (the engine does not bake in
/// scale); tiny has no scene-level transform layer, so `world` is folded
/// into every drawing transform (shape fills / group masks / glyph runs),
/// and length quantities such as blur σ and filter pads are multiplied by
/// `scale` before entering the device-pixel domain.
#[derive(Clone, Copy)]
struct Vec2 {
    x: f32,
    y: f32,
}

/// Draw target: one device-pixel canvas plus its content origin (the device
/// coordinates of the group buffer's top-left corner). The root target has
/// `ox = oy = 0`; a group buffer's origin is always an integer value
/// (`floor(min) − 1`), so group compositing and PlusDarker per-pixel
/// write-back are both integer-pixel aligned.
struct Target {
    pix: Pixmap,
    ox: f32,
    oy: f32,
}

/// Group frame opened by Push\*: blend/opacity/mask + ownership of the
/// parent target (restored on Pop).
struct GroupFrame {
    blend: BlendMode,
    opacity: f32,
    mask: Option<tiny_skia::Mask>,
    parent: Target,
    /// Stack-balance defensive group (placeholder for empty shapes): dropped
    /// directly on Pop without compositing.
    degenerate: bool,
    /// Effect chain applied by PopFilter (remembered by PushFilter; empty
    /// for all other groups).
    filters: Vec<FilterEffect>,
}

/// Renderer state machine (tiny merger of vello's RenderState and the soft
/// state machine).
struct Renderer {
    world: Transform,
    /// Logical→device scale factor (converts blur σ / filter pads to device
    /// pixels).
    scale: f32,
    /// Accumulated scroll translation (PushScroll/PopScroll stack).
    offset: Vec2,
    offset_stack: Vec<Vec2>,
    /// 2D affine stack (PushTransform/PopTransform): composed in screen space
    /// (outermost outside, same order as vello `xforms.push(top ∘ css)`).
    xforms: Vec<Transform>,
    target: Target,
    groups: Vec<GroupFrame>,
}

impl Renderer {
    fn new(width: u32, height: u32, base: [f32; 4], scale: f32) -> Self {
        let mut pix = Pixmap::new(width.max(1), height.max(1)).expect("画布尺寸合法（≥1×1）");
        pix.fill(color_of(base));
        Self {
            world: Transform::from_scale(scale, scale),
            scale,
            offset: Vec2 { x: 0.0, y: 0.0 },
            offset_stack: Vec::new(),
            xforms: Vec::new(),
            target: Target {
                pix,
                ox: 0.0,
                oy: 0.0,
            },
            groups: Vec::new(),
        }
    }

    fn xform(&self) -> Transform {
        *self.xforms.last().unwrap_or(&Transform::identity())
    }

    /// Transform×scroll conjugate (same formula as vello `effective()`):
    /// shape coordinates already have the scroll offset added manually, so
    /// the transform stack must compensate conjugately —
    /// `T(+off) ∘ top ∘ T(−off)`; identity when top is identity (scroll
    /// degenerates to pure translation). The transform×own-scroll ordering
    /// is a Tier C approximation contract (documented in
    /// FEATURES/SINK-MATRIX).
    fn effective(&self) -> Transform {
        let top = self.xform();
        if top.is_identity() {
            return Transform::identity();
        }
        Transform::from_translate(-self.offset.x, -self.offset.y)
            .post_concat(top)
            .post_concat(Transform::from_translate(self.offset.x, self.offset.y))
    }

    /// Op-space point (logical px) → with the scroll offset added.
    fn pt(&self, x: f32, y: f32) -> (f32, f32) {
        (x + self.offset.x, y + self.offset.y)
    }

    /// Shape fill transform: op+offset logical coordinates → target buffer
    /// pixels. `T(−origin) ∘ world ∘ effective` (effective acts on op
    /// coordinates first).
    fn draw_tr(&self) -> Transform {
        self.effective()
            .post_concat(self.world)
            .post_concat(Transform::from_translate(-self.target.ox, -self.target.oy))
    }

    /// Device AABB anchoring the group mask/buffer: the bounding box of the
    /// rect's four corners after `world ∘ effective` (AABB coverage of a
    /// rotated/scaled rect).
    fn device_aabb(&self, x: f32, y: f32, w: f32, h: f32) -> (f32, f32, f32, f32) {
        let (px, py) = self.pt(x, y);
        let m = self.effective().post_concat(self.world);
        let corners = [
            map_pt(m, px, py),
            map_pt(m, px + w, py),
            map_pt(m, px + w, py + h),
            map_pt(m, px, py + h),
        ];
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for c in corners {
            min_x = min_x.min(c.x);
            min_y = min_y.min(c.y);
            max_x = max_x.max(c.x);
            max_y = max_y.max(c.y);
        }
        (min_x, min_y, max_x, max_y)
    }

    fn fill_path(&mut self, path: &Path, paint: &Paint, rule: FillRule) {
        let tr = self.draw_tr();
        self.target.pix.fill_path(path, paint, rule, tr, None);
    }

    #[allow(clippy::too_many_arguments)]
    fn push_group(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: [f32; 8],
        blend: BlendMode,
        opacity: f32,
        filters: Vec<FilterEffect>,
    ) {
        // 缓冲 = 遮罩区域（op+offset 坐标系下给定矩形）的设备 AABB + 1px
        // AA 余量；原点恒整数值（floor−1）→ 组合成整数像素对齐。
        let (min_x, min_y, max_x, max_y) = self.device_aabb(x, y, w, h);
        let ox = min_x.floor() - 1.0;
        let oy = min_y.floor() - 1.0;
        let gw = ((max_x.ceil() - ox) + 1.0).max(1.0);
        let gh = ((max_y.ceil() - oy) + 1.0).max(1.0);
        let pix = Pixmap::new(gw as u32, gh as u32)
            .unwrap_or_else(|| Pixmap::new(1, 1).expect("1×1 画布恒可分配"));
        let mask_tr = self.draw_tr_with_origin(ox, oy);
        let mut mask = match tiny_skia::Mask::new(pix.width(), pix.height()) {
            Some(m) => m,
            None => {
                // 病态尺寸：退化为无遮罩防御组（内容仍入缓冲，组合成不裁剪）。
                let parent = mem::replace(&mut self.target, Target { pix, ox, oy });
                self.groups.push(GroupFrame {
                    blend,
                    opacity,
                    mask: None,
                    parent,
                    degenerate: false,
                    filters,
                });
                return;
            }
        };
        if let Some(path) = rounded_rect_path(x + self.offset.x, y + self.offset.y, w, h, radius) {
            mask.fill_path(&path, FillRule::Winding, true, mask_tr);
        }
        let parent = mem::replace(&mut self.target, Target { pix, ox, oy });
        self.groups.push(GroupFrame {
            blend,
            opacity,
            mask: Some(mask),
            parent,
            degenerate: false,
            filters,
        });
    }

    /// PushClipPath group: polygon mask (nonzero/evenodd rule).
    fn push_group_polygon(&mut self, points: &[[f32; 2]], nonzero: bool) {
        if points.is_empty() {
            // 空多边形=全裁剪：推 1×1 空组保持栈平衡。
            self.push_degenerate_group(BlendMode::Normal, 1.0);
            return;
        }
        let m = self.effective().post_concat(self.world);
        let (mut dmin_x, mut dmin_y, mut dmax_x, mut dmax_y) = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for p in points {
            let (px, py) = self.pt(p[0], p[1]);
            let d = map_pt(m, px, py);
            dmin_x = dmin_x.min(d.x);
            dmin_y = dmin_y.min(d.y);
            dmax_x = dmax_x.max(d.x);
            dmax_y = dmax_y.max(d.y);
        }
        let ox = dmin_x.floor() - 1.0;
        let oy = dmin_y.floor() - 1.0;
        let gw = ((dmax_x.ceil() - ox) + 1.0).max(1.0);
        let gh = ((dmax_y.ceil() - oy) + 1.0).max(1.0);
        let pix = Pixmap::new(gw as u32, gh as u32)
            .unwrap_or_else(|| Pixmap::new(1, 1).expect("1×1 画布恒可分配"));
        let mask_tr = self.draw_tr_with_origin(ox, oy);
        let Some(mut mask) = tiny_skia::Mask::new(pix.width(), pix.height()) else {
            // 病态尺寸：退化防御组。
            let parent = mem::replace(&mut self.target, Target { pix, ox, oy });
            self.groups.push(GroupFrame {
                blend: BlendMode::Normal,
                opacity: 1.0,
                mask: None,
                parent,
                degenerate: false,
                filters: Vec::new(),
            });
            return;
        };
        let mut pb = PathBuilder::new();
        let (fx, fy) = self.pt(points[0][0], points[0][1]);
        pb.move_to(fx, fy);
        for p in &points[1..] {
            let (px, py) = self.pt(p[0], p[1]);
            pb.line_to(px, py);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            let rule = if nonzero {
                FillRule::Winding
            } else {
                FillRule::EvenOdd
            };
            mask.fill_path(&path, rule, true, mask_tr);
        }
        let parent = mem::replace(&mut self.target, Target { pix, ox, oy });
        self.groups.push(GroupFrame {
            blend: BlendMode::Normal,
            opacity: 1.0,
            mask: Some(mask),
            parent,
            degenerate: false,
            filters: Vec::new(),
        });
    }

    /// Stack-balance defensive group (1×1 empty buffer): keeps Push/Pop
    /// paired for empty shapes / pathological sizes.
    fn push_degenerate_group(&mut self, blend: BlendMode, opacity: f32) {
        let pix = Pixmap::new(1, 1).expect("1×1 画布恒可分配");
        let parent = mem::replace(
            &mut self.target,
            Target {
                pix,
                ox: 0.0,
                oy: 0.0,
            },
        );
        self.groups.push(GroupFrame {
            blend,
            opacity,
            mask: None,
            parent,
            degenerate: true,
            filters: Vec::new(),
        });
    }

    /// Builds the drawing transform with an explicit buffer origin (used for
    /// group mask fills).
    fn draw_tr_with_origin(&self, ox: f32, oy: f32) -> Transform {
        self.effective()
            .post_concat(self.world)
            .post_concat(Transform::from_translate(-ox, -oy))
    }

    /// Group pop and compositing (Pop*). When the frame carries a non-empty
    /// effect chain, it is applied to the group buffer first (PopFilter:
    /// demul → soft `apply_effects` → premul, css-filters filter-domain
    /// semantics, single-sourced with soft).
    fn pop_group(&mut self) {
        let Some(g) = self.groups.pop() else {
            return; // 栈失衡防御：无匹配 push 的 pop 直接忽略
        };
        if g.degenerate {
            self.target = g.parent;
            return;
        }
        let (gw, gh) = (self.target.pix.width(), self.target.pix.height());
        let (ox, oy) = (self.target.ox, self.target.oy);
        let group_target = mem::replace(&mut self.target, g.parent);
        let mut pix = group_target.pix;
        // 滤镜链（真模糊，soft 单源）：直排域应用后重建预乘缓冲。
        if !g.filters.is_empty() {
            let (w, h) = (pix.width() as usize, pix.height() as usize);
            let mut buf = pix.take_demultiplied();
            let scaled = scale_effects(&g.filters, self.scale);
            apply_effects(&mut buf, w, h, &scaled);
            pix = IntSize::from_wh(w as u32, h as u32)
                .and_then(|sz| Pixmap::from_vec(buf, sz))
                .unwrap_or_else(|| Pixmap::new(1, 1).expect("1×1 画布恒可分配"));
        }
        // 遮罩内联进组缓冲（DestIn，同尺寸校验恒过——mask 即按该缓冲构建），
        // 再无遮罩合成到父目标：tiny-skia 带遮罩绘制要求 mask 尺寸 == 目标
        // 画布尺寸（RasterPipelineBlitter 尺寸校验不符即静默空绘），而组遮罩
        // 与父画布尺寸天然不等，故不可直接带遮罩 fill_rect。
        if let Some(mask) = g.mask.as_ref() {
            pix.apply_mask(mask);
        }
        // 组合成位置（父 pix 像素系）——原点恒整数 → 整数像素对齐。
        let (px, py) = (ox - self.target.ox, oy - self.target.oy);
        if matches!(g.blend, BlendMode::PlusDarker) {
            self.composite_plus_darker(&pix, px, py, None, g.opacity);
            return;
        }
        // Pattern::new 恒返回 Shader（0.12 签名；pixmap 引用有效即成立）。
        // transform = 瓦片局部→填充坐标：组缓冲 (0,0) 对准合成矩形左上
        // （identity 会把瓦片钉在目标原点，组内容整体位移 −(ox,oy)）。
        let shader = Pattern::new(
            pix.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Nearest,
            g.opacity,
            Transform::from_translate(px, py),
        );
        let paint = Paint {
            shader,
            blend_mode: to_tiny_blend(g.blend),
            anti_alias: false,
            ..Paint::default()
        };
        if let Some(rect) = tiny_skia::Rect::from_xywh(px, py, gw as f32, gh as f32) {
            self.target
                .pix
                .fill_rect(rect, &paint, Transform::identity(), None);
        }
    }

    /// PlusDarker group compositing (tiny-skia has no such mode): the parent
    /// target region is composited per pixel through soft `blend_pixel`
    /// (single source of truth for the formula — soft/lib.rs blend_pixel,
    /// premultiplied addition with a −1 clamp). Group/parent pixels align
    /// 1:1 (integer origin).
    fn composite_plus_darker(
        &mut self,
        group: &Pixmap,
        px: f32,
        py: f32,
        mask: Option<&tiny_skia::Mask>,
        opacity: f32,
    ) {
        let pw = self.target.pix.width() as i32;
        let ph = self.target.pix.height() as i32;
        let gw = group.width() as i32;
        let gh = group.height() as i32;
        // 原点恒整数值 → round 精确无损。
        let (bx, by) = (px.round() as i32, py.round() as i32);
        let x0 = bx.clamp(0, pw);
        let y0 = by.clamp(0, ph);
        let x1 = (bx + gw).clamp(0, pw);
        let y1 = (by + gh).clamp(0, ph);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let md = mask.map(|m| m.data());
        let pd = self.target.pix.data_mut();
        let gd = group.data();
        let msk_w = mask.map(|m| m.width() as i32).unwrap_or(0);
        let msk_h = mask.map(|m| m.height() as i32).unwrap_or(0);
        for yy in y0..y1 {
            let gy = yy - bx;
            if gy < 0 || gy >= gh {
                continue;
            }
            for xx in x0..x1 {
                let gx = xx - bx;
                if gx < 0 || gx >= gw {
                    continue;
                }
                let si = ((gy * gw + gx) * 4) as usize;
                let mut s = demul_px([
                    gd[si] as f32 / 255.0,
                    gd[si + 1] as f32 / 255.0,
                    gd[si + 2] as f32 / 255.0,
                    gd[si + 3] as f32 / 255.0,
                ]);
                if let Some(md) = md {
                    let mi = (gy * msk_w + gx) as usize;
                    if gy < msk_h && gx < msk_w {
                        s[3] *= md[mi] as f32 / 255.0;
                    } else {
                        s[3] = 0.0;
                    }
                }
                s[3] *= opacity;
                let di = ((yy * pw + xx) * 4) as usize;
                let b = demul_px([
                    pd[di] as f32 / 255.0,
                    pd[di + 1] as f32 / 255.0,
                    pd[di + 2] as f32 / 255.0,
                    pd[di + 3] as f32 / 255.0,
                ]);
                let out = blend_pixel(BlendMode::PlusDarker, b, s);
                write_premul_px(&mut pd[di..di + 4], out);
            }
        }
    }

    fn apply_op(&mut self, op: &PaintOp) {
        match op {
            PaintOp::FillRect {
                x,
                y,
                width,
                height,
                radius,
                color,
            } => {
                let (px, py) = self.pt(*x, *y);
                let Some(path) = rounded_rect_path(px, py, *width, *height, *radius) else {
                    return;
                };
                let paint = solid_paint(*color);
                self.fill_path(&path, &paint, FillRule::Winding);
            }
            PaintOp::Gradient {
                x,
                y,
                width,
                height,
                radius,
                gradient,
                radial,
                conic,
                linear,
            } => {
                let (px, py) = self.pt(*x, *y);
                let Some(path) = rounded_rect_path(px, py, *width, *height, *radius) else {
                    return;
                };
                let Some(shader) = gradient_shader(
                    gradient,
                    linear.as_ref(),
                    radial.as_ref(),
                    conic.as_ref(),
                    *x,
                    *y,
                    *width,
                    *height,
                    self,
                ) else {
                    return;
                };
                let paint = Paint {
                    shader,
                    anti_alias: true,
                    ..Paint::default()
                };
                self.fill_path(&path, &paint, FillRule::Winding);
            }
            PaintOp::Shadow { .. } => self.shadow_op(op),
            PaintOp::Image { .. } => self.image_op(op),
            PaintOp::Border { .. } => self.border_op(op),
            PaintOp::PushClip {
                x,
                y,
                width,
                height,
                radius,
            } => self.push_group(
                *x,
                *y,
                *width,
                *height,
                *radius,
                BlendMode::Normal,
                1.0,
                Vec::new(),
            ),
            PaintOp::PushClipPath { points, nonzero } => {
                self.push_group_polygon(points, *nonzero);
            }
            PaintOp::PopClip => self.pop_group(),
            PaintOp::PushOpacity {
                alpha,
                x,
                y,
                width,
                height,
            } => self.push_group(
                *x,
                *y,
                *width,
                *height,
                [0.0; 8],
                BlendMode::Normal,
                *alpha,
                Vec::new(),
            ),
            PaintOp::PopOpacity => self.pop_group(),
            PaintOp::PushBlend {
                mode,
                x,
                y,
                width,
                height,
            } => self.push_group(*x, *y, *width, *height, [0.0; 8], *mode, 1.0, Vec::new()),
            PaintOp::PopBlend => self.pop_group(),
            PaintOp::PushFilter {
                filters,
                x,
                y,
                width,
                height,
            } => {
                // 滤镜区域 = 盒 + effects_pad 外扩（css-filters 滤镜域）；
                // pad 换算设备像素进缓冲，遮罩随之外扩（圆角不加——滤镜域
                // 为矩形外扩，与盒圆角无关）。滤镜链随帧记忆，PopFilter 应用。
                let pad = effects_pad(filters);
                self.push_group(
                    x - pad,
                    y - pad,
                    width + 2.0 * pad,
                    height + 2.0 * pad,
                    [0.0; 8],
                    BlendMode::Normal,
                    1.0,
                    filters.clone(),
                );
            }
            PaintOp::PopFilter => self.pop_group(),
            PaintOp::BackdropFilter { .. } => self.backdrop_op(op),
            PaintOp::PushScroll { dx, dy } => {
                self.offset_stack.push(self.offset);
                self.offset = Vec2 {
                    x: self.offset.x + dx,
                    y: self.offset.y + dy,
                };
            }
            PaintOp::PopScroll => {
                self.offset = self.offset_stack.pop().unwrap_or(Vec2 { x: 0.0, y: 0.0 });
            }
            PaintOp::PushTransform { affine } => {
                // CSS [a,b,c,d,e,f] 直序 → tiny from_row(sx,ky,kx,sy,tx,ty)
                // （tiny-skia-path transform.rs 实证：from_row 参数序与 CSS
                // 同序）；屏幕空间合成：top ∘ css（css 先作用）。
                let css = Transform::from_row(
                    affine[0], affine[1], affine[2], affine[3], affine[4], affine[5],
                );
                let top = self.xform();
                self.xforms.push(top.pre_concat(css));
            }
            PaintOp::PopTransform => {
                self.xforms.pop();
            }
            // Text 基元由 render_with_text 拦截（无文本系统时 no-op，
            // 与 vello render_ops 的 Text 臂同契约）。
            PaintOp::Text { .. } => {}
            _ => {}
        }
    }
}

// ===== 公共 API =====

/// Renders a DisplayList onto an offscreen canvas (no text system; the Text
/// primitive is a no-op, same contract as vello `render_ops`).
pub fn render(list: &DisplayList, width: u32, height: u32, base: [f32; 4], scale: f32) -> Pixmap {
    let mut r = Renderer::new(width, height, base, scale);
    for op in &list.ops {
        r.apply_op(op);
    }
    r.target.pix
}

/// Renders a DisplayList onto an offscreen canvas (with a text system: Text
/// primitives go through parley typesetting + per-glyph skrifa outline
/// fills).
pub fn render_with_text(
    list: &DisplayList,
    width: u32,
    height: u32,
    base: [f32; 4],
    scale: f32,
    text: &mut TinyTextSystem,
) -> Pixmap {
    let mut r = Renderer::new(width, height, base, scale);
    for op in &list.ops {
        match op {
            PaintOp::Text {
                x,
                y,
                text: content,
                color,
                spans,
                font_size,
                font_family,
                font_weight,
                italic,
                max_advance,
                line_height,
                letter_spacing,
                text_align,
                word_break,
                overflow_wrap,
                decorations,
                shadows,
                font_stretch,
                word_spacing,
                font_features,
                font_variations,
            } => r.text_op(
                text,
                *x,
                *y,
                content,
                *color,
                spans,
                *font_size,
                font_family,
                *font_weight,
                *italic,
                *max_advance,
                *line_height,
                *letter_spacing,
                *text_align,
                *word_break,
                *overflow_wrap,
                decorations,
                shadows,
                *font_stretch,
                *word_spacing,
                font_features,
                font_variations,
            ),
            other => r.apply_op(other),
        }
    }
    r.target.pix
}

// ===== 共享助手 =====

/// Straight [f32;4] → tiny Color (sRGB encoded values passed through
/// directly = CSS/soft/vello semantics). Components are clamped to [0,1]
/// (tiny `from_rgba` returns None for out-of-range/NaN, hence the defensive
/// clamp).
fn color_of(c: [f32; 4]) -> Color {
    let v = |x: f32| {
        if x.is_finite() {
            x.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    Color::from_rgba(v(c[0]), v(c[1]), v(c[2]), v(c[3])).unwrap_or(Color::BLACK)
}

/// Applies a Transform to a single point (tiny map_point has an in-place
/// &mut signature; wrapped here as a functional form).
fn map_pt(m: Transform, x: f32, y: f32) -> Point {
    let mut p = Point::from_xy(x, y);
    m.map_point(&mut p);
    p
}

fn solid_paint(color: AlphaColor<Srgb>) -> Paint<'static> {
    let c = color.components;
    Paint {
        shader: Shader::SolidColor(color_of(c)),
        anti_alias: true,
        ..Paint::default()
    }
}

/// Premultiplied RGBA8 → straight [f32;4] (0..1).
fn demul_px(p: [f32; 4]) -> [f32; 4] {
    let a = p[3];
    if a <= 0.0 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    [p[0] / a, p[1] / a, p[2] / a, a]
}

/// Straight [f32;4] → premultiplied RGBA8 written back (rounded, matching
/// tiny-skia premultiply semantics).
fn write_premul_px(dst: &mut [u8], s: [f32; 4]) {
    let a = (s[3] * 255.0).round().clamp(0.0, 255.0) as u16;
    for (c, v) in dst.iter_mut().enumerate().take(3) {
        *v = ((s[c] * a as f32 + 127.0) / 255.0)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    dst[3] = a as u8;
}

/// Core BlendMode → tiny-skia BlendMode (the 16 standard modes 1:1;
/// PlusLighter → tiny `Plus`; PlusDarker is missing → the caller composites
/// per pixel).
fn to_tiny_blend(mode: BlendMode) -> tiny_skia::BlendMode {
    use tiny_skia::BlendMode as T;
    match mode {
        BlendMode::Normal => T::SourceOver,
        BlendMode::Multiply => T::Multiply,
        BlendMode::Screen => T::Screen,
        BlendMode::Overlay => T::Overlay,
        BlendMode::Darken => T::Darken,
        BlendMode::Lighten => T::Lighten,
        BlendMode::ColorDodge => T::ColorDodge,
        BlendMode::ColorBurn => T::ColorBurn,
        BlendMode::HardLight => T::HardLight,
        BlendMode::SoftLight => T::SoftLight,
        BlendMode::Difference => T::Difference,
        BlendMode::Exclusion => T::Exclusion,
        BlendMode::Hue => T::Hue,
        BlendMode::Saturation => T::Saturation,
        BlendMode::Color => T::Color,
        BlendMode::Luminosity => T::Luminosity,
        BlendMode::PlusLighter => T::Plus,
        // tiny-skia 无 PlusDarker：调用方（pop_group）走逐像素合成；
        // 本映射兜底 SourceOver（不可达——pop_group 先行分派）。
        BlendMode::PlusDarker => T::SourceOver,
    }
}

/// Rounded rect path (ported from vello rounded_rect :1185, f64→f32): CSS
/// overlap shrink factor f=min(side/adjacent-radius-sum) → kappa cubic
/// Bézier corner arcs. Returns None when w/h ≤ 0 or all radii are zero
/// (empty path skips the fill).
fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, radius: [f32; 8]) -> Option<Path> {
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let mut r = [
        radius[0].max(0.0),
        radius[1].max(0.0),
        radius[2].max(0.0),
        radius[3].max(0.0),
        radius[4].max(0.0),
        radius[5].max(0.0),
        radius[6].max(0.0),
        radius[7].max(0.0),
    ];
    // CSS 背景与边框 §5.5 重叠收缩：f = min(边长 / 相邻两角半径和)。
    let mut f = 1.0f32;
    for (edge, sum) in [
        (w, r[0] + r[2]),
        (w, r[4] + r[6]),
        (h, r[1] + r[3]),
        (h, r[5] + r[7]),
    ] {
        if sum > edge && sum > 0.0 {
            f = f.min(edge / sum);
        }
    }
    if f < 1.0 {
        for v in &mut r {
            *v *= f;
        }
    }
    let k = 0.552_284_7f32;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r[0], y);
    pb.line_to(x + w - r[2], y);
    pb.cubic_to(
        x + w - r[2] + r[2] * k,
        y,
        x + w,
        y + r[3] - r[3] * k,
        x + w,
        y + r[3],
    );
    pb.line_to(x + w, y + h - r[5]);
    pb.cubic_to(
        x + w,
        y + h - r[5] + r[5] * k,
        x + w - r[4] + r[4] * k,
        y + h,
        x + w - r[4],
        y + h,
    );
    pb.line_to(x + r[6], y + h);
    pb.cubic_to(
        x + r[6] - r[6] * k,
        y + h,
        x,
        y + h - r[7] + r[7] * k,
        x,
        y + h - r[7],
    );
    pb.line_to(x, y + r[1]);
    pb.cubic_to(x, y + r[1] - r[1] * k, x + r[0] - r[0] * k, y, x + r[0], y);
    pb.close();
    pb.finish()
}

// ===== 阴影（真形状遮罩 + soft 盒模糊） =====

impl Renderer {
    /// box-shadow (vello's concentric-ring approximation is upgraded to a
    /// true blur in tiny):
    /// - Outer shadow: shape = the box offset by (dx,dy) and outset by spread
    ///   (corner radii +spread); after the transform stack, take the device
    ///   AABB mask → `blur_alpha_u8` (σ=blur·scale/2, soft single-source
    ///   semantics) → tint and composite;
    /// - Inset shadow: visible region = box coverage ∧ invert(blur(hole)),
    ///   hole = the offset box inset by spread (corner radii −spread) —
    ///   combined per pixel with min.
    fn shadow_op(&mut self, op: &PaintOp) {
        let PaintOp::Shadow {
            x,
            y,
            width,
            height,
            radius,
            color,
            offset_x,
            offset_y,
            blur,
            spread,
            inset,
        } = op
        else {
            return;
        };
        let sigma = blur.max(0.0) * self.scale / 2.0;
        let (px, py) = self.pt(*x, *y);
        let (sx, sy) = (px + offset_x, py + offset_y);
        // 形状矩形（op+offset 逻辑系）与角半径：
        let (shape, sh_radius): ((f32, f32, f32, f32), [f32; 8]) = if *inset {
            // 孔 = offset 后内缩 spread。
            (
                (
                    sx + spread,
                    sy + spread,
                    (width - 2.0 * spread).max(0.0),
                    (height - 2.0 * spread).max(0.0),
                ),
                [
                    (radius[0] - spread).max(0.0),
                    (radius[1] - spread).max(0.0),
                    (radius[2] - spread).max(0.0),
                    (radius[3] - spread).max(0.0),
                    (radius[4] - spread).max(0.0),
                    (radius[5] - spread).max(0.0),
                    (radius[6] - spread).max(0.0),
                    (radius[7] - spread).max(0.0),
                ],
            )
        } else {
            // 外影 = offset 后外扩 spread。
            (
                (
                    sx - spread,
                    sy - spread,
                    width + 2.0 * spread,
                    height + 2.0 * spread,
                ),
                [
                    (radius[0] + spread).max(0.0),
                    (radius[1] + spread).max(0.0),
                    (radius[2] + spread).max(0.0),
                    (radius[3] + spread).max(0.0),
                    (radius[4] + spread).max(0.0),
                    (radius[5] + spread).max(0.0),
                    (radius[6] + spread).max(0.0),
                    (radius[7] + spread).max(0.0),
                ],
            )
        };
        // 缓冲 = 形状设备 AABB + 模糊/偏移余量（1.5·blur = 3σ + 偏移 +
        // spread，逻辑量 ×scale 进设备域）+ 2px AA/采样余量。
        let pad = (1.5 * blur.max(0.0) + offset_x.abs() + offset_y.abs() + spread.abs())
            * self.scale
            + 2.0;
        let (min_x, min_y, max_x, max_y) = self.device_aabb(shape.0, shape.1, shape.2, shape.3);
        let bx = (min_x - pad).floor();
        let by = (min_y - pad).floor();
        let bw = ((max_x + pad).ceil() - bx).max(1.0) as u32;
        let bh = ((max_y + pad).ceil() - by).max(1.0) as u32;
        let mask_tr = self.draw_tr_with_origin(bx, by);
        // 形状覆盖（模糊前）。
        let mut cov = match tiny_skia::Mask::new(bw, bh) {
            Some(m) => m,
            None => return,
        };
        if let Some(path) = rounded_rect_path(shape.0, shape.1, shape.2, shape.3, sh_radius) {
            cov.fill_path(&path, FillRule::Winding, true, mask_tr);
        }
        if sigma > 0.0 {
            blur_alpha_u8(cov.data_mut(), bw as usize, bh as usize, sigma);
        }
        if *inset {
            // 内影 = 盒覆盖 ∧ invert(blur(孔))：逐像素 min 组合。
            let mut box_mask = match tiny_skia::Mask::new(bw, bh) {
                Some(m) => m,
                None => return,
            };
            if let Some(path) = rounded_rect_path(px, py, *width, *height, *radius) {
                box_mask.fill_path(&path, FillRule::Winding, true, mask_tr);
            }
            let bd = box_mask.data_mut();
            let cd = cov.data_mut();
            for i in 0..bd.len() {
                let outside = 255 - cd[i];
                bd[i] = bd[i].min(outside);
            }
            self.composite_shadow_mask(&box_mask, bw, bh, bx, by, color);
        } else {
            self.composite_shadow_mask(&cov, bw, bh, bx, by, color);
        }
    }

    /// Mask → premultiplied shadow-color canvas → composite onto the target
    /// (source-over; box-shadow has no blend semantics). The color α and the
    /// mask coverage fold into a single pass: a_out = a_mask·c_α,
    /// pm = round(straight·255)·a_out/255.
    fn composite_shadow_mask(
        &mut self,
        mask: &tiny_skia::Mask,
        bw: u32,
        bh: u32,
        bx: f32,
        by: f32,
        color: &AlphaColor<Srgb>,
    ) {
        let c = color.components;
        let ca = c[3].clamp(0.0, 1.0);
        let cs8 = [
            (c[0].clamp(0.0, 1.0) * 255.0).round() as u32,
            (c[1].clamp(0.0, 1.0) * 255.0).round() as u32,
            (c[2].clamp(0.0, 1.0) * 255.0).round() as u32,
        ];
        let md = mask.data();
        let mut img = vec![0u8; (bw * bh * 4) as usize];
        for (i, a) in md.iter().enumerate() {
            if *a == 0 {
                continue;
            }
            let ao = (f32::from(*a) * ca).round() as u32;
            let o = i * 4;
            img[o] = (cs8[0] * ao / 255) as u8;
            img[o + 1] = (cs8[1] * ao / 255) as u8;
            img[o + 2] = (cs8[2] * ao / 255) as u8;
            img[o + 3] = ao as u8;
        }
        let Some(shadow_pix) = IntSize::from_wh(bw, bh).and_then(|sz| Pixmap::from_vec(img, sz))
        else {
            return;
        };
        // transform = 瓦片局部→填充坐标（目标局部）：影缓冲 (0,0) 对准
        // (bx−ox, by−oy)，同 pop_group 组合成约定。
        let shader = Pattern::new(
            shadow_pix.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Nearest,
            1.0,
            Transform::from_translate(bx - self.target.ox, by - self.target.oy),
        );
        let paint = Paint {
            shader,
            anti_alias: false,
            ..Paint::default()
        };
        if let Some(rect) = tiny_skia::Rect::from_xywh(
            bx - self.target.ox,
            by - self.target.oy,
            bw as f32,
            bh as f32,
        ) {
            self.target
                .pix
                .fill_rect(rect, &paint, Transform::identity(), None);
        }
    }
}

/// Converts filter-chain length quantities to device pixels (the px fields
/// of Blur/DropShadow; everything else is dimensionless).
fn scale_effects(effects: &[FilterEffect], scale: f32) -> Vec<FilterEffect> {
    effects
        .iter()
        .map(|e| match e {
            FilterEffect::Blur(b) => FilterEffect::Blur(b * scale),
            FilterEffect::DropShadow {
                dx,
                dy,
                blur,
                color,
            } => FilterEffect::DropShadow {
                dx: dx * scale,
                dy: dy * scale,
                blur: blur * scale,
                color: *color,
            },
            other => other.clone(),
        })
        .collect()
}

// ===== 背景滤镜 / 图像 =====

impl Renderer {
    /// backdrop-filter (immediate op, same semantics as soft): snapshot the
    /// current target region → demul → soft `apply_effects` → premul →
    /// write back (replace).
    fn backdrop_op(&mut self, op: &PaintOp) {
        let PaintOp::BackdropFilter {
            filters,
            x,
            y,
            width,
            height,
        } = op
        else {
            return;
        };
        let (min_x, min_y, max_x, max_y) = self.device_aabb(*x, *y, *width, *height);
        let (pw, ph) = (
            self.target.pix.width() as i32,
            self.target.pix.height() as i32,
        );
        let x0 = (min_x.floor() as i32 - self.target.ox as i32).clamp(0, pw);
        let y0 = (min_y.floor() as i32 - self.target.oy as i32).clamp(0, ph);
        let x1 = (max_x.ceil() as i32 - self.target.ox as i32).clamp(0, pw);
        let y1 = (max_y.ceil() as i32 - self.target.oy as i32).clamp(0, ph);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let (rw, rh) = ((x1 - x0) as u32, (y1 - y0) as u32);
        let Some(rect) = tiny_skia::IntRect::from_xywh(x0, y0, rw, rh) else {
            return;
        };
        let Some(region) = self.target.pix.clone_rect(rect) else {
            return;
        };
        let mut buf = region.take_demultiplied();
        let scaled = scale_effects(filters, self.scale);
        apply_effects(&mut buf, rw as usize, rh as usize, &scaled);
        let Some(filtered) = IntSize::from_wh(rw, rh).and_then(|sz| Pixmap::from_vec(buf, sz))
        else {
            return;
        };
        // transform = 瓦片局部→填充坐标（目标局部）：滤镜区域 (0,0) 对准
        // (x0, y0)，同 pop_group 组合成约定。
        let shader = Pattern::new(
            filtered.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Nearest,
            1.0,
            Transform::from_translate(x0 as f32, y0 as f32),
        );
        let paint = Paint {
            shader,
            anti_alias: false,
            ..Paint::default()
        };
        if let Some(rect) = tiny_skia::Rect::from_xywh(x0 as f32, y0 as f32, rw as f32, rh as f32) {
            self.target
                .pix
                .fill_rect(rect, &paint, Transform::identity(), None);
        }
    }

    /// Background image (9-slice affine + Pattern placement): source straight
    /// alpha → premultiplied Pixmap; placement = image pixels → user space
    /// (same direction as the vello brush: img_px → scale(sx,sy) →
    /// translate(px − src_x·sx, py − src_y·sy)); tiny-skia samples through
    /// the inverse. Rounded corners are constrained directly by the path
    /// shape (no clip layer needed).
    fn image_op(&mut self, op: &PaintOp) {
        let PaintOp::Image {
            x,
            y,
            width,
            height,
            radius,
            source_w,
            source_h,
            src_x,
            src_y,
            src_w,
            src_h,
            pixels,
        } = op
        else {
            return;
        };
        if *source_w == 0 || *source_h == 0 {
            return;
        }
        // 直排 sRGBA → 预乘（round(c·a/255)，soft/vello 同式）。
        let mut data = pixels.rgba.as_ref().as_ref().to_vec();
        for px4 in data.as_chunks_mut::<4>().0 {
            let a = px4[3] as u16;
            px4[0] = ((px4[0] as u16 * a + 127) / 255) as u8;
            px4[1] = ((px4[1] as u16 * a + 127) / 255) as u8;
            px4[2] = ((px4[2] as u16 * a + 127) / 255) as u8;
        }
        let Some(img) =
            IntSize::from_wh(*source_w, *source_h).and_then(|sz| Pixmap::from_vec(data, sz))
        else {
            return;
        };
        let (px, py) = self.pt(*x, *y);
        let sx = width / src_w.max(0.001);
        let sy = height / src_h.max(0.001);
        // 图像像素 p → 用户：T(px − src_x·sx, py − src_y·sy) ∘ S(sx, sy)。
        let placement = Transform::from_scale(sx, sy)
            .post_concat(Transform::from_translate(px - src_x * sx, py - src_y * sy));
        let shader = Pattern::new(
            img.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Bilinear,
            1.0,
            placement,
        );
        let Some(path) = rounded_rect_path(px, py, *width, *height, *radius) else {
            return;
        };
        let paint = Paint {
            shader,
            anti_alias: true,
            ..Paint::default()
        };
        self.fill_path(&path, &paint, FillRule::Winding);
    }
}

// ===== 渐变（vello peniko_gradient/repeating_peniko/distribute_stops 端口
// + conic mod 修复 + 径向椭圆 placement） =====

/// Stop table (position normalization + colors + hint expansion; core
/// algorithm single-sourced from P9-1a).
fn gradient_stops(g: &Gradient, line_len: f32) -> Vec<(f32, AlphaColor<Srgb>)> {
    use style_engine::css::property::{apply_gradient_hints, distribute_stop_positions};
    use style_engine::css::value::ColorValue;
    let raw: Vec<Option<f32>> = g
        .stops
        .iter()
        .map(|s| match s.position {
            Some(LengthPercentage::Px(v)) => Some(if line_len > 0.0 {
                (v / line_len).clamp(0.0, 1.0)
            } else {
                0.0
            }),
            Some(LengthPercentage::Percent(f)) => Some(f.clamp(0.0, 1.0)),
            // em/rem/cq/vw 等无渐变线上下文的单位：无位停点参与均布。
            _ => None,
        })
        .collect();
    let positions = distribute_stop_positions(&raw);
    let mut stops: Vec<(f32, AlphaColor<Srgb>)> = g
        .stops
        .iter()
        .zip(positions)
        .map(|(s, p)| {
            let c = match s.color {
                ColorValue::Absolute(ac) => ac,
                // 非 Absolute 终结防御：不透明黑（P9-1a 双 sink 统一）。
                _ => AlphaColor::new([0.0, 0.0, 0.0, 1.0]),
            };
            (p, c)
        })
        .collect();
    let hints: Vec<(usize, f32)> = g
        .hints
        .iter()
        .filter_map(|h| match h.position {
            LengthPercentage::Percent(f) => Some((h.after_stop, f.clamp(0.0, 1.0))),
            LengthPercentage::Px(v) => Some((
                h.after_stop,
                if line_len > 0.0 {
                    (v / line_len).clamp(0.0, 1.0)
                } else {
                    0.0
                },
            )),
            // em/rem/cq 提示丢弃（B·豁免，ADR-0038 D5）。
            _ => None,
        })
        .collect();
    if !hints.is_empty() {
        stops = apply_gradient_hints(&stops, &hints);
    }
    stops
}

/// Gradient shader construction (op box x/y/w/h in logical coordinates; the
/// renderer supplies the scroll offset).
#[allow(clippy::too_many_arguments)]
fn gradient_shader(
    g: &Gradient,
    linear: Option<&LinearGeom>,
    radial: Option<&RadialGeom>,
    conic: Option<&ConicGeom>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: &Renderer,
) -> Option<Shader<'static>> {
    let (px, py) = r.pt(x, y);
    // 几何 + 线长（vello peniko_gradient 缺几何回退同式）。
    enum Geom {
        Linear { p0: Point, p1: Point, len: f32 },
        Radial { c: Point, radius: f32, rx: f32 },
        Conic { c: Point, start_deg: f32 },
    }
    let geom = match (linear, radial, conic) {
        (Some(lg), _, _) => {
            let p0 = Point::from_xy(lg.start[0] + r.offset.x, lg.start[1] + r.offset.y);
            let p1 = Point::from_xy(lg.end[0] + r.offset.x, lg.end[1] + r.offset.y);
            let len = ((p1.x - p0.x).powi(2) + (p1.y - p0.y).powi(2))
                .sqrt()
                .max(1e-6);
            Geom::Linear { p0, p1, len }
        }
        (_, Some(rg), _) => {
            let c = Point::from_xy(rg.cx + r.offset.x, rg.cy + r.offset.y);
            let radius = rg.ry.max(0.5);
            Geom::Radial {
                c,
                radius,
                rx: rg.rx,
            }
        }
        (_, _, Some(cg)) => {
            let c = Point::from_xy(cg.cx + r.offset.x, cg.cy + r.offset.y);
            // CSS from D：ConicGeom.start = (D−90°) 弧度；tiny sweep 角度
            // 域 [0,1) turns 接缝在 +X（CSS 90°）→ start 归一 [0,360)
            // （conic 破案定案：mod 语义 + Repeat 采样）。
            let start_deg = cg.start.to_degrees().rem_euclid(360.0);
            Geom::Conic { c, start_deg }
        }
        _ => match &g.kind {
            GradientKind::Linear(angle) => {
                let rad = angle.0.to_radians();
                let dir = (rad.sin(), -rad.cos());
                let line = (w * dir.0.abs() + h * dir.1.abs()) / 2.0;
                let c = Point::from_xy(px + w / 2.0, py + h / 2.0);
                let p0 = Point::from_xy(c.x - dir.0 * line, c.y - dir.1 * line);
                let p1 = Point::from_xy(c.x + dir.0 * line, c.y + dir.1 * line);
                Geom::Linear {
                    p0,
                    p1,
                    len: (2.0 * line).max(1e-6),
                }
            }
            GradientKind::Radial(_) => {
                let diag = ((w / 2.0).powi(2) + (h / 2.0).powi(2)).sqrt();
                Geom::Radial {
                    c: Point::from_xy(px + w / 2.0, py + h / 2.0),
                    radius: diag.max(0.5),
                    rx: diag.max(0.5),
                }
            }
            GradientKind::Conic(_) => Geom::Conic {
                c: Point::from_xy(px + w / 2.0, py + h / 2.0),
                start_deg: -90.0f32.rem_euclid(360.0),
            },
            // non_exhaustive 兜底：透明黑。
            _ => {
                return Some(Shader::SolidColor(Color::TRANSPARENT));
            }
        },
    };

    // repeating 周期收缩（vello repeating_peniko 同式）。
    let to_tiny_stops = |stops: &[(f32, AlphaColor<Srgb>)]| -> Vec<GradientStop> {
        stops
            .iter()
            .map(|(p, c)| GradientStop::new(*p, color_of(c.components)))
            .collect()
    };
    let mut stops = gradient_stops(
        g,
        match &geom {
            Geom::Linear { len, .. } => *len,
            Geom::Radial { radius, .. } => *radius,
            Geom::Conic { .. } => 1.0,
        },
    );
    if stops.len() < 2 {
        return Some(Shader::SolidColor(Color::TRANSPARENT));
    }
    let (first, last) = (stops[0].0, stops[stops.len() - 1].0);
    let period = last - first;
    if g.repeating {
        if period <= 1e-6 {
            return Some(Shader::SolidColor(Color::TRANSPARENT));
        }
        stops = stops
            .iter()
            .map(|(p, c)| ((p - first) / period, *c))
            .collect();
    }
    let tiny_stops = to_tiny_stops(&stops);
    match geom {
        Geom::Linear { p0, p1, len } => {
            if g.repeating {
                // 端点收缩到 [first, first+period] 周期段。
                let ux = (p1.x - p0.x) / len;
                let uy = (p1.y - p0.y) / len;
                let s2 = Point::from_xy(p0.x + ux * first * len, p0.y + uy * first * len);
                let e2 = Point::from_xy(s2.x + ux * period * len, s2.y + uy * period * len);
                LinearGradient::new(
                    s2,
                    e2,
                    tiny_stops,
                    SpreadMode::Repeat,
                    Transform::identity(),
                )
            } else {
                LinearGradient::new(p0, p1, tiny_stops, SpreadMode::Pad, Transform::identity())
            }
        }
        Geom::Radial { c, radius, rx } => {
            // 椭圆修正：placement = 渐变空间→用户（brush→user，peniko 同向）
            // T(c)·S(rx/ry,1)·T(−c)——采样取逆后用户 x 距 rx 恰达渐变缘
            // ry；rx≈ry 恒等。
            let transform = if (rx - radius).abs() > 0.01 && radius > 0.0 {
                let s = rx / radius;
                Transform::from_translate(c.x, c.y)
                    .pre_concat(Transform::from_scale(s, 1.0))
                    .pre_concat(Transform::from_translate(-c.x, -c.y))
            } else {
                Transform::identity()
            };
            if g.repeating {
                RadialGradient::new(
                    c,
                    first * radius,
                    c,
                    (first + period) * radius,
                    tiny_stops,
                    SpreadMode::Repeat,
                    transform,
                )
            } else {
                RadialGradient::new(c, 0.0, c, radius, tiny_stops, SpreadMode::Pad, transform)
            }
        }
        Geom::Conic { c, start_deg } => {
            // conic 恒 Repeat：fract 采样 = mod 语义（Chrome 同）；
            // tiny「start≤0&&end≥360 强制 Pad」仅 start_deg=0 触发，该处
            // t'=phi∈[0,1) 恒区间内，Pad≡Repeat 无害。
            let (s2, e2) = if g.repeating {
                (
                    start_deg + 360.0 * first,
                    start_deg + 360.0 * (first + period),
                )
            } else {
                (start_deg, start_deg + 360.0)
            };
            SweepGradient::new(
                c,
                s2,
                e2,
                tiny_stops,
                SpreadMode::Repeat,
                Transform::identity(),
            )
        }
    }
}

// ===== 边框（vello draw_border 几何 f64→f32 端口） =====

/// Shared parameters for corner-arc boundary-angle computation: corner
/// radius / centerline arc radius / effective widths of the two adjacent
/// sides. See the identically named function in the vello sink (formulas and
/// semantic notes migrated one by one).
fn corner_diagonal_deg(w_u: f32, w_v: f32) -> f32 {
    w_v.atan2(w_u).to_degrees()
}

fn diagonal_arc_crossing_deg(r: f32, rc: f32, w_u: f32, w_v: f32) -> f32 {
    let len = (w_u * w_u + w_v * w_v).sqrt();
    if len <= 0.0 {
        return 180.0;
    }
    let phi = corner_diagonal_deg(w_u, w_v).to_radians();
    let (a, b) = (phi.cos(), phi.sin());
    let d = rc * rc - r * r * (a - b) * (a - b);
    let t = if d >= 0.0 {
        (r * (a + b) - d.sqrt()).max(0.0)
    } else {
        r * (a + b)
    };
    let angle = (t * b - r).atan2(t * a - r).to_degrees().rem_euclid(360.0);
    angle.clamp(180.0, 270.0)
}

fn centerline_radius(r: f32, w: f32) -> f32 {
    (r - w / 2.0).max(0.5)
}

/// Boundary angle between the two arc segments at a corner (screen
/// coordinates, shift = TL 0°/TR 90°/BR 180°/BL 270°): returns (neighbor
/// segment end angle, owner segment start angle).
fn corner_arc_bounds(r: f32, w_owner: f32, w_neighbor: f32, shift_deg: f32) -> (f32, f32) {
    let th_n = diagonal_arc_crossing_deg(r, centerline_radius(r, w_neighbor), w_neighbor, w_owner);
    let th_o = diagonal_arc_crossing_deg(r, centerline_radius(r, w_owner), w_neighbor, w_owner);
    (
        (th_n + shift_deg).rem_euclid(360.0),
        (th_o + shift_deg).rem_euclid(360.0),
    )
}

/// css-backgrounds-3 §4.5 corner-arc overlap shrinking.
fn shrink_corner_radii(w: f32, h: f32, radius: [f32; 4]) -> [f32; 4] {
    let [tl, tr, br, bl] = radius;
    let mut f = 1.0f32;
    for (edge_len, sum) in [(w, tl + tr), (h, tr + br), (w, br + bl), (h, bl + tl)] {
        if sum > edge_len {
            f = f.min(edge_len / sum);
        }
    }
    if f < 1.0 {
        [tl * f, tr * f, br * f, bl * f]
    } else {
        radius
    }
}

/// Starts a new segment on the arc (move_to; y-down, so increasing θ = clockwise on screen).
fn arc_move_to(pb: &mut PathBuilder, cx: f32, cy: f32, r: f32, deg: f32) {
    let a = deg.to_radians();
    pb.move_to(cx + a.cos() * r, cy + a.sin() * r);
}

/// Cubic Bézier approximation of an arc over an arbitrary span (spans >90°
/// are split into ≤90° pieces, k=4/3·tan(Δθ/4)).
fn arc_segment(pb: &mut PathBuilder, cx: f32, cy: f32, r: f32, start_deg: f32, end_deg: f32) {
    let span = (end_deg - start_deg).to_radians();
    if span.abs() < 1e-9 {
        return;
    }
    let steps = (span.abs() / core::f32::consts::FRAC_PI_2).ceil().max(1.0);
    let step = span / steps;
    let k = (4.0 / 3.0) * (step / 4.0).tan();
    let half_pi = core::f32::consts::FRAC_PI_2;
    let mut a = start_deg.to_radians();
    for _ in 0..steps as u32 {
        let a2 = a + step;
        let (p0x, p0y) = (a.cos(), a.sin());
        let (p3x, p3y) = (a2.cos(), a2.sin());
        let (t1x, t1y) = ((a + half_pi).cos(), (a + half_pi).sin());
        let (t2x, t2y) = ((a2 + half_pi).cos(), (a2 + half_pi).sin());
        pb.cubic_to(
            cx + (p0x + t1x * k) * r,
            cy + (p0y + t1y * k) * r,
            cx + (p3x - t2x * k) * r,
            cy + (p3y - t2y * k) * r,
            cx + p3x * r,
            cy + p3y * r,
        );
        a = a2;
    }
}

impl Renderer {
    fn fill_tri(&mut self, pts: [[f32; 2]; 3], color: AlphaColor<Srgb>) {
        let mut pb = PathBuilder::new();
        pb.move_to(pts[0][0], pts[0][1]);
        pb.line_to(pts[1][0], pts[1][1]);
        pb.line_to(pts[2][0], pts[2][1]);
        pb.close();
        let Some(path) = pb.finish() else {
            return;
        };
        let paint = solid_paint(color);
        self.fill_path(&path, &paint, FillRule::Winding);
    }

    fn fill_quad(&mut self, sq: [f32; 4], color: AlphaColor<Srgb>) {
        let [x0, y0, x1, y1] = sq;
        self.fill_tri([[x0, y0], [x1, y0], [x1, y1]], color);
        self.fill_tri([[x0, y0], [x1, y1], [x0, y1]], color);
    }

    fn stroke_side(&mut self, path: &Path, s: &style_engine::paint::BorderSide) {
        use style_engine::css::property::BorderStyle;
        if s.style == BorderStyle::None || s.width <= 0.0 {
            return;
        }
        let mut stroke = tiny_skia::Stroke {
            width: s.width,
            ..tiny_skia::Stroke::default()
        };
        match s.style {
            BorderStyle::Dashed => {
                stroke.dash = tiny_skia::StrokeDash::new(vec![s.width * 3.0], 0.0);
            }
            BorderStyle::Dotted => {
                stroke.line_cap = tiny_skia::LineCap::Round;
                stroke.dash = tiny_skia::StrokeDash::new(vec![0.0, s.width * 2.0], 0.0);
            }
            _ => {}
        }
        let paint = solid_paint(s.color);
        let tr = self.draw_tr();
        self.target.pix.stroke_path(path, &paint, &stroke, tr, None);
    }

    /// Per-side drawing (line-by-line port of vello draw_border): square
    /// corners split the square along the diagonal, rounded corners stroke
    /// the centerline of "arc segment + straight line" per corner.
    fn border_op(&mut self, op: &PaintOp) {
        use style_engine::css::property::BorderStyle;
        let PaintOp::Border {
            x,
            y,
            width,
            height,
            radius,
            sides,
        } = op
        else {
            return;
        };
        let (px, py) = self.pt(*x, *y);
        let (w, h) = (*width, *height);
        // 角取横半径（vello 调用点同式：[r0, r2, r4, r6]）。
        let [tl, tr, br, bl] =
            shrink_corner_radii(w, h, [radius[0], radius[2], radius[4], radius[6]]);
        let (wt, wr, wb, wl) = (
            sides[0].width,
            sides[1].width,
            sides[2].width,
            sides[3].width,
        );
        let on =
            |s: &style_engine::paint::BorderSide| s.style != BorderStyle::None && s.width > 0.0;
        let we = [
            if on(&sides[0]) { wt } else { 0.0 },
            if on(&sides[1]) { wr } else { 0.0 },
            if on(&sides[2]) { wb } else { 0.0 },
            if on(&sides[3]) { wl } else { 0.0 },
        ];
        let bounds = [
            corner_arc_bounds(tl, we[0], we[3], 0.0),
            corner_arc_bounds(tr, we[1], we[0], 90.0),
            corner_arc_bounds(br, we[2], we[1], 180.0),
            corner_arc_bounds(bl, we[3], we[2], 270.0),
        ];
        // 方角角部方块：(圆角判定, 拥有边, 相邻边, 方块, 拥有 tri, 相邻 tri)。
        type CornerEntry = (f32, usize, usize, [f32; 4], [[f32; 2]; 3], [[f32; 2]; 3]);
        let corners: [CornerEntry; 4] = [
            (
                tl,
                0,
                3,
                [px, py, px + wl, py + wt],
                [[px, py], [px + wl, py], [px + wl, py + wt]],
                [[px, py], [px + wl, py + wt], [px, py + wt]],
            ),
            (
                tr,
                1,
                0,
                [px + w - wr, py, px + w, py + wt],
                [[px + w, py], [px + w, py + wt], [px + w - wr, py + wt]],
                [[px + w, py], [px + w - wr, py + wt], [px + w - wr, py]],
            ),
            (
                br,
                2,
                1,
                [px + w - wr, py + h - wb, px + w, py + h],
                [
                    [px + w, py + h],
                    [px + w - wr, py + h],
                    [px + w - wr, py + h - wb],
                ],
                [
                    [px + w, py + h],
                    [px + w - wr, py + h - wb],
                    [px + w, py + h - wb],
                ],
            ),
            (
                bl,
                3,
                2,
                [px, py + h - wb, px + wl, py + h],
                [[px, py + h], [px, py + h - wb], [px + wl, py + h - wb]],
                [[px, py + h], [px + wl, py + h - wb], [px + wl, py + h]],
            ),
        ];
        for (r, owner, other, sq, tri_owner, tri_other) in corners {
            if r > 0.0 {
                continue;
            }
            let (o, t) = (&sides[owner], &sides[other]);
            match (on(o), on(t)) {
                (false, false) => {}
                (true, false) => self.fill_quad(sq, o.color),
                (false, true) => self.fill_quad(sq, t.color),
                (true, true) if o.color == t.color => self.fill_quad(sq, o.color),
                (true, true) => {
                    self.fill_tri(tri_owner, o.color);
                    self.fill_tri(tri_other, t.color);
                }
            }
        }
        // top：TL 拥有段 [θo,270°] + TR 邻接段 [270°,θn]。
        let mut pb = PathBuilder::new();
        if tl > 0.0 && wt > 0.0 {
            let rc = centerline_radius(tl, wt);
            arc_move_to(&mut pb, px + tl, py + tl, rc, bounds[0].1);
            arc_segment(&mut pb, px + tl, py + tl, rc, bounds[0].1, 270.0);
        } else {
            pb.move_to(px + wl, py + wt / 2.0);
        }
        pb.line_to(
            if tr > 0.0 { px + w - tr } else { px + w - wr },
            py + wt / 2.0,
        );
        if tr > 0.0 && wt > 0.0 {
            let rc = centerline_radius(tr, wt);
            arc_segment(&mut pb, px + w - tr, py + tr, rc, 270.0, bounds[1].0);
        }
        if let Some(path) = pb.finish() {
            self.stroke_side(&path, &sides[0]);
        }
        // right：TR 拥有段 [θo,360°] + BR 邻接段 [0°,θn]。
        let mut pb = PathBuilder::new();
        if tr > 0.0 && wr > 0.0 {
            let rc = centerline_radius(tr, wr);
            arc_move_to(&mut pb, px + w - tr, py + tr, rc, bounds[1].1);
            arc_segment(&mut pb, px + w - tr, py + tr, rc, bounds[1].1, 360.0);
        } else {
            pb.move_to(px + w - wr / 2.0, py + wt);
        }
        pb.line_to(
            px + w - wr / 2.0,
            if br > 0.0 { py + h - br } else { py + h - wb },
        );
        if br > 0.0 && wr > 0.0 {
            let rc = centerline_radius(br, wr);
            arc_segment(&mut pb, px + w - br, py + h - br, rc, 0.0, bounds[2].0);
        }
        if let Some(path) = pb.finish() {
            self.stroke_side(&path, &sides[1]);
        }
        // bottom：BR 拥有段 [θo,90°] + BL 邻接段 [90°,θn]。
        let mut pb = PathBuilder::new();
        if br > 0.0 && wb > 0.0 {
            let rc = centerline_radius(br, wb);
            arc_move_to(&mut pb, px + w - br, py + h - br, rc, bounds[2].1);
            arc_segment(&mut pb, px + w - br, py + h - br, rc, bounds[2].1, 90.0);
        } else {
            pb.move_to(px + w - wr, py + h - wb / 2.0);
        }
        pb.line_to(if bl > 0.0 { px + bl } else { px + wl }, py + h - wb / 2.0);
        if bl > 0.0 && wb > 0.0 {
            let rc = centerline_radius(bl, wb);
            arc_segment(&mut pb, px + bl, py + h - bl, rc, 90.0, bounds[3].0);
        }
        if let Some(path) = pb.finish() {
            self.stroke_side(&path, &sides[2]);
        }
        // left：BL 拥有段 [θo,180°] + TL 邻接段 [180°,θn]。
        let mut pb = PathBuilder::new();
        if bl > 0.0 && wl > 0.0 {
            let rc = centerline_radius(bl, wl);
            arc_move_to(&mut pb, px + bl, py + h - bl, rc, bounds[3].1);
            arc_segment(&mut pb, px + bl, py + h - bl, rc, bounds[3].1, 180.0);
        } else {
            pb.move_to(px + wl / 2.0, py + h - wb);
        }
        pb.line_to(px + wl / 2.0, if tl > 0.0 { py + tl } else { py + wt });
        if tl > 0.0 && wl > 0.0 {
            let rc = centerline_radius(tl, wl);
            arc_segment(&mut pb, px + tl, py + tl, rc, 180.0, bounds[0].0);
        }
        if let Some(path) = pb.finish() {
            self.stroke_side(&path, &sides[3]);
        }
    }
}

// ===== 文本（vello draw_text parley 排版 port + skrifa 逐字形轮廓填充） =====

/// Sink-side text system (zero side effects: system fonts disabled, fonts are
/// pushed in by the host — same contract as vello `VelloTextSystem`, with
/// identical source bytes on both conformance sides).
pub struct TinyTextSystem {
    font_cx: parley::FontContext,
    layout_cx: parley::LayoutContext<()>,
}

impl Default for TinyTextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl TinyTextSystem {
    pub fn new() -> Self {
        Self {
            font_cx: parley::FontContext {
                collection: parley::fontique::Collection::new(
                    parley::fontique::CollectionOptions {
                        system_fonts: false,
                        ..parley::fontique::CollectionOptions::default()
                    },
                ),
                source_cache: parley::fontique::SourceCache::default(),
            },
            layout_cx: parley::LayoutContext::default(),
        }
    }

    pub fn add_font(&mut self, data: Vec<u8>) {
        self.font_cx
            .collection
            .register_fonts(parley::fontique::Blob::new(Arc::new(data)), None);
    }
}

/// Alignment mapping (same form as vello map_align; non_exhaustive falls back
/// to Start).
fn map_align(a: TextAlign) -> parley::layout::Alignment {
    match a {
        TextAlign::Start => parley::layout::Alignment::Start,
        TextAlign::End => parley::layout::Alignment::End,
        TextAlign::Center => parley::layout::Alignment::Center,
        TextAlign::Left => parley::layout::Alignment::Left,
        TextAlign::Right => parley::layout::Alignment::Right,
        TextAlign::Justify => parley::layout::Alignment::Justify,
        _ => parley::layout::Alignment::Start,
    }
}

/// Family-name normalization (same mapping as core text.rs / vello family_of).
fn family_of(list: &FontFamilyList) -> std::borrow::Cow<'static, str> {
    match list.0.iter().next() {
        Some(FamilyName::Named(s)) => s.clone().into(),
        Some(FamilyName::Serif) => "serif".into(),
        Some(FamilyName::SansSerif) => "sans-serif".into(),
        Some(FamilyName::Monospace) => "monospace".into(),
        Some(FamilyName::Cursive) => "cursive".into(),
        Some(FamilyName::Fantasy) => "fantasy".into(),
        Some(FamilyName::SystemUi) => "system-ui".into(),
        Some(_) => "sans-serif".into(),
        None => "sans-serif".into(),
    }
}

/// Y-flipping pen from skrifa outlines to tiny paths (skrifa outlines are
/// y-up, the screen is y-down; outline coordinates are already scaled by
/// ppem).
struct YFlipPen<'a> {
    pb: &'a mut PathBuilder,
}

impl skrifa::outline::OutlinePen for YFlipPen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(x, -y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(x, -y);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.pb.quad_to(cx0, -cy0, x, -y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.pb.cubic_to(cx0, -cy0, cx1, -cy1, x, -y);
    }
    fn close(&mut self) {
        self.pb.close();
    }
}

/// Single-glyph outline path (skrifa unhinted draw; variation axes are
/// instantiated via `location`).
fn glyph_outline(
    font: &skrifa::FontRef,
    gid: u32,
    font_size: f32,
    coords: &[skrifa::prelude::NormalizedCoord],
) -> Option<Path> {
    use skrifa::outline::DrawSettings;
    use skrifa::prelude::{GlyphId, LocationRef, MetadataProvider, Size};
    let og = font.outline_glyphs().get(GlyphId::new(gid))?;
    let mut pb = PathBuilder::new();
    let settings = DrawSettings::unhinted(Size::new(font_size), LocationRef::new(coords));
    let mut pen = YFlipPen { pb: &mut pb };
    og.draw(settings, &mut pen).ok()?;
    pb.finish()
}

/// Text-decoration rectangular band (port of vello fill_deco_rect):
/// [x0,x1]×[y−half,y+half] under `tr` (the run transform into the target
/// buffer).
fn fill_deco_rect(
    r: &mut Renderer,
    color: AlphaColor<Srgb>,
    tr: Transform,
    x0: f32,
    x1: f32,
    y: f32,
    half: f32,
) {
    let Some(path) = rounded_rect_path(x0, y - half, x1 - x0, 2.0 * half, [0.0; 8]) else {
        return;
    };
    let paint = solid_paint(color);
    let full = tr
        .post_concat(r.world)
        .post_concat(Transform::from_translate(-r.target.ox, -r.target.oy));
    r.target
        .pix
        .fill_path(&path, &paint, FillRule::Winding, full, None);
}

/// Text-decoration wavy band (port of vello fill_deco_wavy): period 6t,
/// amplitude 2t, 8 segments per period, band thickness translated along the
/// wave; top edge out, bottom edge translated by t and closed back.
fn fill_deco_wavy(
    r: &mut Renderer,
    color: AlphaColor<Srgb>,
    tr: Transform,
    x0: f32,
    x1: f32,
    cy: f32,
    t: f32,
) {
    let period = 6.0 * t;
    let amp = 2.0 * t;
    let seg = period / 8.0;
    let len = x1 - x0;
    let wavy_y = |u: f32| cy + amp * (core::f32::consts::TAU * u / period).sin();
    let mut top: Vec<f32> = Vec::new();
    let mut u = 0.0f32;
    while u < len {
        top.push(u);
        u += seg;
    }
    top.push(len);
    let mut pb = PathBuilder::new();
    pb.move_to(x0, wavy_y(0.0) - t * 0.5);
    for &u in &top {
        pb.line_to(x0 + u, wavy_y(u) - t * 0.5);
    }
    for &u in top.iter().rev() {
        pb.line_to(x0 + u, wavy_y(u) + t * 0.5);
    }
    pb.close();
    let Some(path) = pb.finish() else {
        return;
    };
    let paint = solid_paint(color);
    let full = tr
        .post_concat(r.world)
        .post_concat(Transform::from_translate(-r.target.ox, -r.target.oy));
    r.target
        .pix
        .fill_path(&path, &paint, FillRule::Winding, full, None);
}

impl Renderer {
    /// Text primitive (port of vello draw_text with its 21 parameters).
    /// Glyphs = per-glyph skrifa outline fills (avoiding winding interaction
    /// within a run, same shape as soft); text shadow = glyph coverage mask +
    /// true blur (vello's concentric-ring approximation is upgraded here).
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::type_complexity)]
    fn text_op(
        &mut self,
        text: &mut TinyTextSystem,
        x: f32,
        y: f32,
        content: &str,
        color: AlphaColor<Srgb>,
        spans: &[style_engine::paint::TextSpanPaint],
        font_size: f32,
        font_family: &FontFamilyList,
        font_weight: f32,
        italic: bool,
        max_advance: Option<f32>,
        line_height: Option<f32>,
        letter_spacing: f32,
        text_align: TextAlign,
        word_break: WordBreakKind,
        overflow_wrap: OverflowWrapKind,
        decorations: &[style_engine::paint::TextDecorationPaint],
        shadows: &[style_engine::paint::TextShadowPaint],
        font_stretch: f32,
        word_spacing: Option<f32>,
        font_features: &[([u8; 4], u16)],
        font_variations: &[([u8; 4], f32)],
    ) {
        if content.is_empty() {
            return;
        }
        // run_transform（vello 同式）：origin = top·(x,y)（原始变换，无偏
        // 移）；run_tr = T(offset + origin) ∘ top——字形局部点 p → top(p)
        // → +offset+origin（滚动偏移设备空间后置，与 effective 共轭同契）。
        let top = self.xform();
        let o = map_pt(top, x, y);
        let run_tr =
            Transform::from_translate(self.offset.x + o.x, self.offset.y + o.y).pre_concat(top);
        // run 空间 → 目标缓冲像素。
        let text_tr = run_tr
            .post_concat(self.world)
            .post_concat(Transform::from_translate(-self.target.ox, -self.target.oy));

        // ---- 排版（vello push_default 全序 port） ----
        let mut builder = text
            .layout_cx
            .ranged_builder(&mut text.font_cx, content, 1.0, false);
        use parley::style::StyleProperty;
        builder.push_default(StyleProperty::FontSize(font_size));
        builder.push_default(StyleProperty::FontWeight(
            parley::fontique::FontWeight::new(font_weight),
        ));
        builder.push_default(parley::FontFamily::Source(family_of(font_family)));
        if italic {
            builder.push_default(StyleProperty::FontStyle(
                parley::fontique::FontStyle::Italic,
            ));
        }
        if let Some(lh) = line_height {
            builder.push_default(StyleProperty::LineHeight(
                parley::style::LineHeight::Absolute(lh),
            ));
        }
        if letter_spacing != 0.0 {
            builder.push_default(StyleProperty::LetterSpacing(letter_spacing));
        }
        builder.push_default(StyleProperty::WordBreak(match word_break {
            WordBreakKind::Normal => parley::style::WordBreak::Normal,
            WordBreakKind::BreakAll => parley::style::WordBreak::BreakAll,
            WordBreakKind::KeepAll => parley::style::WordBreak::KeepAll,
            _ => parley::style::WordBreak::Normal,
        }));
        builder.push_default(StyleProperty::OverflowWrap(match overflow_wrap {
            OverflowWrapKind::Normal => parley::style::OverflowWrap::Normal,
            OverflowWrapKind::BreakWord => parley::style::OverflowWrap::BreakWord,
            OverflowWrapKind::Anywhere => parley::style::OverflowWrap::Anywhere,
            _ => parley::style::OverflowWrap::Normal,
        }));
        if font_stretch != 100.0 {
            builder.push_default(StyleProperty::FontWidth(
                parley::fontique::FontWidth::from_percentage(font_stretch),
            ));
        }
        if let Some(ws) = word_spacing {
            builder.push_default(StyleProperty::WordSpacing(ws));
        }
        if !font_features.is_empty() {
            let fl: Vec<parley::FontFeature> = font_features
                .iter()
                .map(|(tag, v)| parley::FontFeature::new(parley::setting::Tag::new(tag), *v))
                .collect();
            builder.push_default(StyleProperty::FontFeatures(parley::FontFeatures::List(
                std::borrow::Cow::Owned(fl),
            )));
        }
        if !font_variations.is_empty() {
            let vl: Vec<parley::FontVariation> = font_variations
                .iter()
                .map(|(tag, v)| parley::FontVariation::new(parley::setting::Tag::new(tag), *v))
                .collect();
            builder.push_default(StyleProperty::FontVariations(parley::FontVariations::List(
                std::borrow::Cow::Owned(vl),
            )));
        }
        for s in spans {
            let range = (s.start as usize)..(s.end as usize).min(content.len());
            builder.push(StyleProperty::FontSize(s.font_size), range.clone());
            builder.push(
                StyleProperty::FontWeight(parley::fontique::FontWeight::new(s.font_weight)),
                range.clone(),
            );
            builder.push(
                parley::FontFamily::Source(family_of(&s.font_family)),
                range.clone(),
            );
            if s.italic {
                builder.push(
                    StyleProperty::FontStyle(parley::fontique::FontStyle::Italic),
                    range.clone(),
                );
            }
            // P9-5（ADR-0042）：行高仅非 normal（Some）时推；字距恒推。
            if let Some(lh) = s.line_height {
                builder.push(
                    StyleProperty::LineHeight(parley::style::LineHeight::Absolute(lh)),
                    range.clone(),
                );
            }
            builder.push(StyleProperty::LetterSpacing(s.letter_spacing), range);
        }
        let mut layout = builder.build(content);
        // parley 0.11 break_all_lines 直接收 Option<f32>（None=不折行）。
        layout.break_all_lines(max_advance);
        if !matches!(text_align, TextAlign::Start) {
            layout.align(
                map_align(text_align),
                parley::layout::AlignmentOptions::default(),
            );
        }

        // ---- 文本影（真模糊：字形覆盖 → 遮罩 → blur → 着色合成） ----
        if !shadows.is_empty() {
            let (tw, th) = (self.target.pix.width(), self.target.pix.height());
            for sh in shadows {
                let Some(mut m) = tiny_skia::Mask::new(tw, th) else {
                    continue;
                };
                let sigma = sh.blur.max(0.0) * self.scale / 2.0;
                // 影偏移折入遮罩填充变换（先偏移后模糊 ≡ 线性平移等价）。
                let shadow_tr = Transform::from_translate(sh.dx * self.scale, sh.dy * self.scale)
                    .post_concat(text_tr);
                self.fill_glyph_coverages(
                    &layout,
                    spans,
                    color,
                    font_variations,
                    shadow_tr,
                    Some(&mut m),
                );
                if sigma > 0.0 {
                    blur_alpha_u8(m.data_mut(), tw as usize, th as usize, sigma);
                }
                self.composite_shadow_mask(&m, tw, th, 0.0, 0.0, &sh.color);
            }
        }

        // ---- 主绘制（逐 run：字体解析 → 逐字形轮廓 fill） ----
        self.fill_glyph_coverages(&layout, spans, color, font_variations, text_tr, None);

        // ---- 装饰线（vello 行级聚合 port） ----
        if !decorations.is_empty() {
            for line in layout.lines() {
                let mut min_x = f32::INFINITY;
                let mut max_x = f32::NEG_INFINITY;
                let mut baseline = 0.0f32;
                let mut ascent = 0.0f32;
                let mut descent = 0.0f32;
                let mut seen = false;
                for item in line.items() {
                    let parley::layout::PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                        continue;
                    };
                    let run = glyph_run.run();
                    let mut gx = f32::INFINITY;
                    for g in glyph_run.positioned_glyphs() {
                        gx = gx.min(g.x);
                        baseline = baseline.max(g.y);
                    }
                    min_x = min_x.min(gx);
                    max_x = max_x.max(gx + run.advance());
                    let m = run.metrics();
                    ascent = ascent.max(m.ascent);
                    descent = descent.max(m.descent);
                    seen = true;
                }
                if !seen {
                    continue;
                }
                for d in decorations.iter() {
                    let t = d.thickness_px.max(0.5);
                    let mut line_ys = Vec::new();
                    if d.line & 1 != 0 {
                        line_ys.push(baseline + descent * 0.5);
                    }
                    if d.line & 2 != 0 {
                        line_ys.push(baseline - ascent * 0.9);
                    }
                    if d.line & 4 != 0 {
                        line_ys.push(baseline - ascent * 0.5);
                    }
                    for cy in line_ys {
                        match d.style {
                            TextDecoStyleKind::Wavy => {
                                fill_deco_wavy(self, d.color, text_tr, min_x, max_x, cy, t);
                            }
                            TextDecoStyleKind::Double => {
                                fill_deco_rect(
                                    self,
                                    d.color,
                                    text_tr,
                                    min_x,
                                    max_x,
                                    cy - t * 0.75,
                                    t * 0.25,
                                );
                                fill_deco_rect(
                                    self,
                                    d.color,
                                    text_tr,
                                    min_x,
                                    max_x,
                                    cy + t * 0.25,
                                    t * 0.25,
                                );
                            }
                            _ => {
                                fill_deco_rect(self, d.color, text_tr, min_x, max_x, cy, t * 0.5);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Layout glyph traversal and filling: when `mask` is Some, only the
    /// coverage is drawn (text-shadow channel, untinted); when None, the
    /// font is resolved per run and filled with span colors (main channel).
    /// Variation axes follow the op-level `font_variations` (Text contract:
    /// base style level).
    fn fill_glyph_coverages(
        &mut self,
        layout: &parley::Layout<()>,
        spans: &[style_engine::paint::TextSpanPaint],
        color: AlphaColor<Srgb>,
        font_variations: &[([u8; 4], f32)],
        text_tr: Transform,
        mut mask: Option<&mut tiny_skia::Mask>,
    ) {
        for line in layout.lines() {
            for item in line.items() {
                let parley::layout::PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                    continue;
                };
                let run = glyph_run.run();
                let run_range = run.text_range();
                let run_color = if mask.is_none() {
                    spans
                        .iter()
                        .rev()
                        .find(|s| {
                            (s.start as usize) < run_range.end && run_range.start < (s.end as usize)
                        })
                        .map(|s| s.color)
                        .unwrap_or(color)
                } else {
                    color // 影通道：颜色不参与（遮罩只收覆盖）
                };
                // 字体解析：fontique FontData（run.font() = linebender
                // resource handle：data: Blob<u8> + index: u32）→ skrifa。
                let info = run.font();
                let Ok(font) = skrifa::FontRef::from_index(info.data.data(), info.index) else {
                    continue;
                };
                // 变体轴实例化：op 级 font_variations → 该字体的轴域归一
                // （skrifa AxisCollection::location；未知 tag 忽略、缺省
                // 轴 = 0.0）。
                let coords: Vec<skrifa::prelude::NormalizedCoord> = {
                    use skrifa::prelude::MetadataProvider;
                    if font_variations.is_empty() {
                        Vec::new()
                    } else {
                        let tags: Vec<(&str, f32)> = font_variations
                            .iter()
                            .filter_map(|(tag, v)| {
                                std::str::from_utf8(&tag[..]).ok().map(|s| (s, *v))
                            })
                            .collect();
                        font.axes().location(tags).coords().to_vec()
                    }
                };
                for g in glyph_run.positioned_glyphs() {
                    let Some(path) = glyph_outline(&font, g.id, run.font_size(), &coords) else {
                        continue;
                    };
                    let g_tr = Transform::from_translate(g.x, g.y).post_concat(text_tr);
                    match mask.as_deref_mut() {
                        Some(m) => {
                            m.fill_path(&path, FillRule::Winding, true, g_tr);
                        }
                        None => {
                            let paint = solid_paint(run_color);
                            self.target
                                .pix
                                .fill_path(&path, &paint, FillRule::Winding, g_tr, None);
                        }
                    }
                }
            }
        }
    }
}
