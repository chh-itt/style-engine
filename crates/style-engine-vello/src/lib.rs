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
use vello::kurbo::{Affine, BezPath, Point, Stroke, Vec2};
use vello::peniko::color::{AlphaColor, Srgb};
use vello::peniko::{Brush, Extend, Fill, Gradient, GradientKind, Mix, RadialGradientPosition};

/// 将绘制清单写入 vello 场景（追加语义；调用方持有场景生命周期）。
pub fn render_ops(list: &DisplayList, scene: &mut Scene) {
    let mut state = RenderState::default();
    for op in &list.ops {
        apply_op(op, scene, &mut state);
    }
}

/// 带文本绘制形态：Text 基元经 sink 侧 parley 排版 + DrawGlyphs 落字形。
pub fn render_ops_with_text(list: &DisplayList, scene: &mut Scene, text: &mut VelloTextSystem) {
    let mut state = RenderState::default();
    for op in &list.ops {
        if let PaintOp::Text {
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
        } = op
        {
            // 字形为局部簇坐标（positioned_glyphs 已含 advance 与基线）：
            // run 变换 = translate(偏移 + T·盒原点) ∘ T（T = 变换栈顶）。
            // T 恒等时即原有 translate(偏移 + 盒原点)，行为不变。
            let top = state.xform();
            let origin = top * Point::new(f64::from(*x), f64::from(*y));
            let run_transform =
                Affine::translate((state.offset.x + origin.x, state.offset.y + origin.y)) * top;
            text.draw_text(
                scene,
                run_transform,
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
            );
            continue;
        }
        apply_op(op, scene, &mut state);
    }
}

/// text-align → parley Alignment（第五批⑳；变体一一对应）。
fn map_align(a: style_engine::css::property::TextAlign) -> parley::layout::Alignment {
    use style_engine::css::property::TextAlign as T;
    match a {
        T::Start => parley::layout::Alignment::Start,
        T::End => parley::layout::Alignment::End,
        T::Center => parley::layout::Alignment::Center,
        T::Left => parley::layout::Alignment::Left,
        T::Right => parley::layout::Alignment::Right,
        T::Justify => parley::layout::Alignment::Justify,
    }
}

/// 家族名归一（与 core text.rs 同一映射）。
fn family_of(list: &style_engine::css::property::FontFamilyList) -> std::borrow::Cow<'static, str> {
    match list.0.iter().next() {
        Some(style_engine::css::property::FamilyName::Named(s)) => s.clone().into(),
        Some(style_engine::css::property::FamilyName::Serif) => "serif".into(),
        Some(style_engine::css::property::FamilyName::SansSerif) => "sans-serif".into(),
        Some(style_engine::css::property::FamilyName::Monospace) => "monospace".into(),
        Some(style_engine::css::property::FamilyName::Cursive) => "cursive".into(),
        Some(style_engine::css::property::FamilyName::Fantasy) => "fantasy".into(),
        Some(style_engine::css::property::FamilyName::SystemUi) => "system-ui".into(),
        None => "sans-serif".into(),
    }
}

/// sink 侧文本系统（零副作用：系统字体禁用，字体由宿主推入）。
pub struct VelloTextSystem {
    font_cx: parley::FontContext,
    layout_cx: parley::LayoutContext<()>,
}

impl Default for VelloTextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl VelloTextSystem {
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
            .register_fonts(parley::fontique::Blob::new(std::sync::Arc::new(data)), None);
    }

    /// 绘制单个 Text 基元（run_transform = 偏移/变换合成后的字形 run 变换；
    /// 无字体时为无字形 no-op）。
    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &mut self,
        scene: &mut Scene,
        run_transform: Affine,
        content: &str,
        color: AlphaColor<Srgb>,
        spans: &[style_engine::paint::TextSpanPaint],
        font_size: f32,
        font_family: &style_engine::css::property::FontFamilyList,
        font_weight: f32,
        italic: bool,
        max_advance: Option<f32>,
        line_height: Option<f32>,
        letter_spacing: f32,
        text_align: style_engine::css::property::TextAlign,
    ) {
        if content.is_empty() {
            return;
        }
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, content, 1.0, false);
        builder.push_default(parley::style::StyleProperty::FontSize(font_size));
        builder.push_default(parley::style::StyleProperty::FontWeight(
            parley::fontique::FontWeight::new(font_weight),
        ));
        builder.push_default(parley::FontFamily::Source(family_of(font_family)));
        if italic {
            builder.push_default(parley::style::StyleProperty::FontStyle(
                parley::fontique::FontStyle::Italic,
            ));
        }
        // 行高/字距（盘点修复）：与测量侧同源（PaintOp 携带解析值），保证折行一致
        if let Some(lh) = line_height {
            builder.push_default(parley::style::StyleProperty::LineHeight(
                parley::style::LineHeight::Absolute(lh),
            ));
        }
        if letter_spacing != 0.0 {
            builder.push_default(parley::style::StyleProperty::LetterSpacing(letter_spacing));
        }
        // span 覆盖样式（T5c）：字节区间 [start, end)
        for s in spans {
            let range = (s.start as usize)..(s.end as usize).min(content.len());
            builder.push(
                parley::style::StyleProperty::FontSize(s.font_size),
                range.clone(),
            );
            builder.push(
                parley::style::StyleProperty::FontWeight(parley::fontique::FontWeight::new(
                    s.font_weight,
                )),
                range.clone(),
            );
            builder.push(
                parley::FontFamily::Source(family_of(&s.font_family)),
                range.clone(),
            );
            if s.italic {
                builder.push(
                    parley::style::StyleProperty::FontStyle(parley::fontique::FontStyle::Italic),
                    range,
                );
            }
        }
        let mut layout = builder.build(content);
        // 与测量共用同一 max_advance（T5c-2）：保证折行一致
        layout.break_all_lines(max_advance);
        // text-align（第五批⑳）：折行后行内对齐；对齐宽 = 排版时记录的
        // 可用宽（= max_advance，与 CSS 内容盒语义一致），不改盒宽（测量
        // 侧无需对齐）。Start 跳过 = 排版器默认行为；Justify 末行起始对齐。
        if !matches!(text_align, style_engine::css::property::TextAlign::Start) {
            layout.align(
                map_align(text_align),
                parley::layout::AlignmentOptions::default(),
            );
        }
        for line in layout.lines() {
            for item in line.items() {
                let parley::layout::PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                    continue;
                };
                let run = glyph_run.run();
                let font = run.font().clone();
                // run → span 颜色：取与 run 文本区间重叠的最后一个 span（T5c）
                let run_range = run.text_range();
                let run_color = spans
                    .iter()
                    .rev()
                    .find(|s| {
                        (s.start as usize) < run_range.end && run_range.start < (s.end as usize)
                    })
                    .map(|s| s.color)
                    .unwrap_or(color);
                // positioned_glyphs 已累计 advance 并并入 run 偏移与基线（line.rs:235），
                // 故变换只需 run_transform（= 偏移/变换合成，见 render_ops 调用点）；
                // glyphs() 是簇相对坐标，直接用会让整行字形叠在一点。
                scene
                    .draw_glyphs(&font)
                    .font_size(run.font_size())
                    .transform(run_transform)
                    .brush(run_color)
                    .draw(
                        Fill::NonZero,
                        glyph_run.positioned_glyphs().map(|g| vello::Glyph {
                            id: g.id,
                            x: g.x,
                            y: g.y,
                        }),
                    );
            }
        }
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
    /// 2D 仿射栈（ADR-0009 PushTransform/PopTransform）：屏幕空间合成，
    /// 与手动叠加的偏移平移正交（形状坐标已偏移、见 effective() 共轭）。
    xforms: Vec<Affine>,
}

impl RenderState {
    /// 变换栈顶（无变换层 = 恒等）。
    fn xform(&self) -> Affine {
        self.xforms.last().copied().unwrap_or(Affine::IDENTITY)
    }

    /// 形状类绘制的 per-call 变换：形状构造时已手动叠加偏移平移，
    /// 故变换层须与当前偏移共轭——eff(v) = offset + T·(v − offset)。
    /// 无变换层时返回恒等（与变换前行为逐位一致）。
    /// 已知近似：同一节点上 transform × 自身滚动（CSS 语义滚动在变换内）
    /// 的次序由偏移共轭近似，记录于 FEATURES。
    fn effective(&self) -> Affine {
        let top = self.xform();
        if top == Affine::IDENTITY {
            return Affine::IDENTITY;
        }
        Affine::translate(Vec2::new(self.offset.x, self.offset.y))
            * top
            * Affine::translate(Vec2::new(-self.offset.x, -self.offset.y))
    }
}

/// 圆角矩形路径（第五批⑪椭圆圆角）：radius = 每角 (横, 纵)——tl.x tl.y
/// tr.x tr.y br.x br.y bl.x bl.y；x==y 时即圆形角。四分之一椭圆以 kappa
/// cubic 逼近；半径按 CSS 重叠规则等比缩放（任一边上相邻两角半径和超过
/// 边长时全组乘 f），负值截断。
fn rounded_rect(x: f32, y: f32, w: f32, h: f32, radius: [f32; 8]) -> BezPath {
    let raw = [
        (radius[0], radius[1]),
        (radius[2], radius[3]),
        (radius[4], radius[5]),
        (radius[6], radius[7]),
    ];
    let nonneg: Vec<(f64, f64)> = raw
        .iter()
        .map(|(a, b)| (f64::from(a.max(0.0)), f64::from(b.max(0.0))))
        .collect();
    let (tl, tr_, br_, bl) = (nonneg[0], nonneg[1], nonneg[2], nonneg[3]);
    // CSS 重叠缩放：f = min(1, 各边 边长/相邻两角半径和 的最小值)
    let mut f = 1.0f64;
    for (edge, sum) in [
        (f64::from(w), tl.0 + tr_.0),
        (f64::from(w), bl.0 + br_.0),
        (f64::from(h), tl.1 + bl.1),
        (f64::from(h), tr_.1 + br_.1),
    ] {
        if sum > edge && sum > 0.0 {
            f = f.min(edge / sum);
        }
    }
    let (tl, tr_, br_, bl) = (
        (tl.0 * f, tl.1 * f),
        (tr_.0 * f, tr_.1 * f),
        (br_.0 * f, br_.1 * f),
        (bl.0 * f, bl.1 * f),
    );
    let (x, y, w, h) = (f64::from(x), f64::from(y), f64::from(w), f64::from(h));
    let k = 0.552_284_749_830_793_6_f64; // 4/3·tan(π/8)：四分之一椭圆 cubic 逼近
    let mut p = BezPath::new();
    p.move_to((x + tl.0, y));
    p.line_to((x + w - tr_.0, y));
    p.curve_to(
        (x + w - tr_.0 + k * tr_.0, y),
        (x + w, y + tr_.1 - k * tr_.1),
        (x + w, y + tr_.1),
    );
    p.line_to((x + w, y + h - br_.1));
    p.curve_to(
        (x + w, y + h - br_.1 + k * br_.1),
        (x + w - br_.0 + k * br_.0, y + h),
        (x + w - br_.0, y + h),
    );
    p.line_to((x + bl.0, y + h));
    p.curve_to(
        (x + bl.0 - k * bl.0, y + h),
        (x, y + h - bl.1 + k * bl.1),
        (x, y + h - bl.1),
    );
    p.line_to((x, y + tl.1));
    p.curve_to(
        (x, y + tl.1 - k * tl.1),
        (x + tl.0 - k * tl.0, y),
        (x + tl.0, y),
    );
    p.close_path();
    p
}

fn rect_shape(x: f32, y: f32, w: f32, h: f32, radius: [f32; 8]) -> BezPath {
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
            scene.fill(Fill::NonZero, state.effective(), *color, None, &shape);
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
            let shape = rect_shape(
                *x + state.offset.x as f32,
                *y + state.offset.y as f32,
                *width,
                *height,
                *radius,
            );
            let brush = peniko_gradient(gradient, *radial, *x, *y, *width, *height, state);
            // 椭圆修正（T4c）：rx≠ry 时对画刷施加以圆心为锚的 x 向缩放
            let brush_transform = radial.and_then(|gm| {
                if gm.ry > 0.0 && (gm.rx - gm.ry).abs() > 0.01 {
                    let sx = f64::from(gm.rx) / f64::from(gm.ry);
                    let c = Point::new(
                        f64::from(gm.cx) + state.offset.x,
                        f64::from(gm.cy) + state.offset.y,
                    );
                    Some(
                        Affine::translate((c.x, c.y))
                            * Affine::scale_non_uniform(sx, 1.0)
                            * Affine::translate((-c.x, -c.y)),
                    )
                } else {
                    None
                }
            });
            scene.fill(
                Fill::NonZero,
                state.effective(),
                Brush::Gradient(&brush),
                brush_transform,
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
            scene.fill(Fill::NonZero, state.effective(), *color, None, &shape);
        }
        PaintOp::Border {
            x,
            y,
            width,
            height,
            radius,
            sides,
        } => {
            draw_border(
                scene,
                *x + state.offset.x as f32,
                *y + state.offset.y as f32,
                *width,
                *height,
                // 第五批⑪：边框条角部取横半径（椭圆圆角的边框条暂以圆形角近似）
                [radius[0], radius[2], radius[4], radius[6]],
                sides,
                state.effective(),
            );
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
            scene.push_layer(Fill::NonZero, Mix::Normal, 1.0, state.effective(), &shape);
        }
        PaintOp::PopClip => {
            scene.pop_layer();
        }
        PaintOp::PushOpacity {
            alpha,
            x,
            y,
            width,
            height,
        } => {
            let shape = rect_shape(
                *x + state.offset.x as f32,
                *y + state.offset.y as f32,
                *width,
                *height,
                [0.0; 8],
            );
            scene.push_layer(
                Fill::NonZero,
                Mix::Normal,
                *alpha,
                state.effective(),
                &shape,
            );
        }
        PaintOp::PopOpacity => {
            scene.pop_layer();
        }
        PaintOp::PushScroll { dx, dy } => {
            state.stack.push(state.offset);
            state.offset += Vec2::new(f64::from(*dx), f64::from(*dy));
        }
        PaintOp::PopScroll => {
            state.offset = state.stack.pop().unwrap_or(Vec2::ZERO);
        }
        PaintOp::PushTransform { affine } => {
            // 屏幕空间合成：A·B 中 B 先应用（外层变换在外）
            let top = state.xform();
            state.xforms.push(
                top * Affine::new([
                    f64::from(affine[0]),
                    f64::from(affine[1]),
                    f64::from(affine[2]),
                    f64::from(affine[3]),
                    f64::from(affine[4]),
                    f64::from(affine[5]),
                ]),
            );
        }
        PaintOp::PopTransform => {
            state.xforms.pop();
        }
        // PaintOp #[non_exhaustive]：后续基元先忽略
        _ => {}
    }
}

/// 引擎 Gradient → peniko Gradient（CSS 渐变线几何；stop 缺省位置均匀分配）。
fn peniko_gradient(
    g: &style_engine::css::property::Gradient,
    radial: Option<style_engine::paint::RadialGeom>,
    x: f32,
    y: f32,
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
            // 画刷与形状同处用户空间（形状坐标已叠盒原点+偏移）：中心须含盒原点
            // （第四批⑤修复：此前漏加 (x,y)，非原点盒的线性渐变采样区错位）
            let cx = f64::from(x) + f64::from(w) / 2.0 + state.offset.x;
            let cy = f64::from(y) + f64::from(h) / 2.0 + state.offset.y;
            let start = Point::new(cx - dir.x * line, cy - dir.y * line);
            let end = Point::new(cx + dir.x * line, cy + dir.y * line);
            Gradient::new_linear(start, end)
        }
        style_engine::css::property::GradientKind::Radial(_) => {
            // T4c：圆心/半径已在 paint 层解析为绝对值（含盒原点）；缺失时退回盒心对角线近似
            let diag =
                (((f64::from(w) / 2.0).powi(2) + (f64::from(h) / 2.0).powi(2)).sqrt()) as f32;
            let geom = radial.unwrap_or(style_engine::paint::RadialGeom {
                cx: x + w * 0.5,
                cy: y + h * 0.5,
                rx: diag,
                ry: diag,
            });
            Gradient {
                kind: GradientKind::Radial(RadialGradientPosition::new(
                    Point::new(
                        f64::from(geom.cx) + state.offset.x,
                        f64::from(geom.cy) + state.offset.y,
                    ),
                    geom.ry.max(0.5),
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

/// 90° 圆弧的三次贝塞尔近似（y-down，θ 递增 = 屏幕顺时针；起点须已 move_to）。
fn quarter_arc(path: &mut BezPath, cx: f64, cy: f64, r: f64, start_deg: f64) {
    let k = 0.552_284_7;
    let (a1, a2) = (start_deg.to_radians(), (start_deg + 90.0).to_radians());
    let (p0x, p0y) = (a1.cos(), a1.sin());
    let (p3x, p3y) = (a2.cos(), a2.sin());
    let half_pi = core::f64::consts::FRAC_PI_2;
    let (t1x, t1y) = ((a1 + half_pi).cos(), (a1 + half_pi).sin());
    let (t2x, t2y) = ((a2 + half_pi).cos(), (a2 + half_pi).sin());
    path.curve_to(
        Point::new(cx + (p0x + t1x * k) * r, cy + (p0y + t1y * k) * r),
        Point::new(cx + (p3x - t2x * k) * r, cy + (p3y - t2y * k) * r),
        Point::new(cx + p3x * r, cy + p3y * r),
    );
}

fn stroke_side(
    scene: &mut Scene,
    path: &BezPath,
    s: &style_engine::paint::BorderSide,
    xform: Affine,
) {
    use style_engine::css::property::BorderStyle;
    if s.style == BorderStyle::None || s.width <= 0.0 {
        return;
    }
    let mut stroke = Stroke::new(f64::from(s.width));
    match s.style {
        BorderStyle::Dashed => {
            stroke = stroke.with_dashes(0.0, [f64::from(s.width) * 3.0]);
        }
        BorderStyle::Dotted => {
            stroke = stroke
                .with_caps(vello::kurbo::Cap::Round)
                .with_dashes(0.0, [0.0, f64::from(s.width) * 2.0]);
        }
        _ => {}
    }
    scene.stroke(&stroke, xform, s.color, None, path);
}

/// 方角（radius≈0）角部对角线二分（第四批⑤）：外角→内角对角线把角部方块
/// 分给相邻两边（CSS 语义）；单边存在整块归该边；同色整块一次填充——消除
/// 旧「全边长直线交叉」的半透明双重着色与「后画方」角色偏差。圆角仍走
/// 角弧（归属不变）；不等宽圆角弧起点不随邻边带宽调整——近似记 FEATURES。
fn fill_tri(scene: &mut Scene, xform: Affine, pts: [[f32; 2]; 3], color: AlphaColor<Srgb>) {
    let mut path = BezPath::new();
    path.move_to(Point::new(f64::from(pts[0][0]), f64::from(pts[0][1])));
    for p in &pts[1..] {
        path.line_to(Point::new(f64::from(p[0]), f64::from(p[1])));
    }
    path.close_path();
    scene.fill(Fill::NonZero, xform, color, None, &path);
}

fn fill_quad(scene: &mut Scene, xform: Affine, sq: [f32; 4], color: AlphaColor<Srgb>) {
    let [x0, y0, x1, y1] = sq;
    fill_tri(scene, xform, [[x0, y0], [x1, y0], [x1, y1]], color);
    fill_tri(scene, xform, [[x0, y0], [x1, y1], [x0, y1]], color);
}

/// 方角角部条目：(圆角判定, 拥有边, 相邻边, 方块, 拥有边三角, 相邻边三角)。
type CornerSpec = (f32, usize, usize, [f32; 4], [[f32; 2]; 3], [[f32; 2]; 3]);

/// 四边分画（T4b）：每边一条「角弧 + 直线」描边路径；角弧按顺时针归属
/// （TL→top、TR→right、BR→bottom、BL→left）。
#[allow(clippy::too_many_arguments)]
fn draw_border(
    scene: &mut Scene,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: [f32; 4],
    sides: &[style_engine::paint::BorderSide; 4],
    xform: Affine,
) {
    use style_engine::css::property::BorderStyle;
    let [tl, tr, br, bl] = radius;
    let (wt, wr, wb, wl) = (
        sides[0].width,
        sides[1].width,
        sides[2].width,
        sides[3].width,
    );
    let f = f64::from;
    let on = |s: &style_engine::paint::BorderSide| s.style != BorderStyle::None && s.width > 0.0;

    // 方角角部方块：(圆角判定, 拥有边, 相邻边, 方块, 拥有边三角, 相邻边三角)
    let corners: [CornerSpec; 4] = [
        (
            tl,
            0,
            3,
            [x, y, x + wl, y + wt],
            [[x, y], [x + wl, y], [x + wl, y + wt]],
            [[x, y], [x + wl, y + wt], [x, y + wt]],
        ),
        (
            tr,
            1,
            0,
            [x + w - wr, y, x + w, y + wt],
            [[x + w, y], [x + w, y + wt], [x + w - wr, y + wt]],
            [[x + w, y], [x + w - wr, y + wt], [x + w - wr, y]],
        ),
        (
            br,
            2,
            1,
            [x + w - wr, y + h - wb, x + w, y + h],
            [
                [x + w, y + h],
                [x + w - wr, y + h],
                [x + w - wr, y + h - wb],
            ],
            [
                [x + w, y + h],
                [x + w - wr, y + h - wb],
                [x + w, y + h - wb],
            ],
        ),
        (
            bl,
            3,
            2,
            [x, y + h - wb, x + wl, y + h],
            [[x, y + h], [x, y + h - wb], [x + wl, y + h - wb]],
            [[x, y + h], [x + wl, y + h - wb], [x + wl, y + h]],
        ),
    ];
    for (r, owner, other, sq, tri_owner, tri_other) in corners {
        if r > 0.0 {
            continue;
        }
        let (o, t) = (&sides[owner], &sides[other]);
        match (on(o), on(t)) {
            (false, false) => {}
            (true, false) => fill_quad(scene, xform, sq, o.color),
            (false, true) => fill_quad(scene, xform, sq, t.color),
            (true, true) if o.color == t.color => fill_quad(scene, xform, sq, o.color),
            (true, true) => {
                fill_tri(scene, xform, tri_owner, o.color);
                fill_tri(scene, xform, tri_other, t.color);
            }
        }
    }

    // top（TL 弧 / TL 方块右缘起）
    let mut p = BezPath::new();
    if tl > 0.0 && wt > 0.0 {
        let rc = f((tl - wt / 2.0).max(0.5));
        p.move_to(Point::new(f(x + tl) - rc, f(y + tl)));
        quarter_arc(&mut p, f(x + tl), f(y + tl), rc, 180.0);
    } else {
        p.move_to(Point::new(f(x + wl), f(y + wt / 2.0)));
    }
    p.line_to(Point::new(
        if tr > 0.0 {
            f(x + w - tr)
        } else {
            f(x + w - wr)
        },
        f(y + wt / 2.0),
    ));
    stroke_side(scene, &p, &sides[0], xform);

    // right（TR 弧 / TR 方块下缘起）
    let mut p = BezPath::new();
    if tr > 0.0 && wr > 0.0 {
        let rc = f((tr - wr / 2.0).max(0.5));
        p.move_to(Point::new(f(x + w - tr), f(y + tr) - rc));
        quarter_arc(&mut p, f(x + w - tr), f(y + tr), rc, 270.0);
    } else {
        p.move_to(Point::new(f(x + w - wr / 2.0), f(y + wt)));
    }
    p.line_to(Point::new(
        f(x + w - wr / 2.0),
        if br > 0.0 {
            f(y + h - br)
        } else {
            f(y + h - wb)
        },
    ));
    stroke_side(scene, &p, &sides[1], xform);

    // bottom（BR 弧 / BR 方块左缘起）
    let mut p = BezPath::new();
    if br > 0.0 && wb > 0.0 {
        let rc = f((br - wb / 2.0).max(0.5));
        p.move_to(Point::new(f(x + w - br) + rc, f(y + h - br)));
        quarter_arc(&mut p, f(x + w - br), f(y + h - br), rc, 0.0);
    } else {
        p.move_to(Point::new(f(x + w - wr), f(y + h - wb / 2.0)));
    }
    p.line_to(Point::new(
        if bl > 0.0 { f(x + bl) } else { f(x + wl) },
        f(y + h - wb / 2.0),
    ));
    stroke_side(scene, &p, &sides[2], xform);

    // left（BL 弧 / BL 方块上缘起）
    let mut p = BezPath::new();
    if bl > 0.0 && wl > 0.0 {
        let rc = f((bl - wl / 2.0).max(0.5));
        p.move_to(Point::new(f(x + bl), f(y + h - bl) + rc));
        quarter_arc(&mut p, f(x + bl), f(y + h - bl), rc, 90.0);
    } else {
        p.move_to(Point::new(f(x + wl / 2.0), f(y + h - wb)));
    }
    p.line_to(Point::new(
        f(x + wl / 2.0),
        if tl > 0.0 { f(y + tl) } else { f(y + wt) },
    ));
    stroke_side(scene, &p, &sides[3], xform);
}
