//! `style-engine-soft` — DisplayList 的纯软件绘制后端（第二 Sink，第五批㉛）。
//!
//! 职责：验证 DisplayList 契约的 **sink 无关性**——零 GPU、零第三方依赖
//! （纯 Rust 标准库软件光栅化），像素确定性可作 CI 级绘制断言的参照实现。
//!
//! 合成语义（与 ㉕ 探针实测对齐）：sRGB 编码值直接 src-over（=vello 0.10
//! Rgba8Unorm 路径与 Chromium/CSS 默认一致）；渐变停点 sRGB 插值
//! （CSS 默认插值空间）。
//!
//! ⑦ 转正覆盖矩阵：
//!
//! | op | 支持 | 备注 |
//! |---|---|---|
//! | FillRect | ✓ | 椭圆圆角逐像素覆盖测试 |
//! | Gradient | ✓ | linear（CSS 角度）+ radial（RadialGeom 椭圆）+ conic（ConicGeom 扫角，C3）；停点色仅 Absolute（其余视作全透明），位置仅 Px/Percent（其余/None 自动均布） |
//! | Shadow | ✓ blur 路径 | blur=0：外扩/内缩平移矩形（原路径）；blur>0：真形状遮罩（圆角矩形 out/inset）+ 3×盒模糊≈高斯（σ=blur/2、pad=⌈3σ⌉、整数滑动窗确定性）——仅纯平移矩阵，旋转/缩放回退平移矩形（记录偏差） |
//! | Image | ✓ | 最近邻采样（缩放无滤波） |
//! | Border | 近似 | 直边带（Solid；Dashed/Dotted 近似为实线；圆角未斜切） |
//! | PushClip/PopClip | ✓ | 矩形+圆角裁剪栈（裁剪矩节点随所在变换层） |
//! | PushClipPath/PopClip | ✓ | 多边形裁剪（nonzero/evenodd 射线法，F3c） |
//! | PushOpacity/PopOpacity | ✓ | 有界组 alpha（快照回混，ADR-0008；bbox 随变换层） |
//! | PushBlend/PopBlend | ✓ | 混合组全 18 种模式（快照底 + 清区累积，pop 按模式合成；P1-2，css-compositing-1；plus-lighter/darker=预乘加法惯例） |
//! | PushScroll/PopScroll | ✓ | 平移折叠进变换矩阵（嵌套累加） |
//! | PushTransform/PopTransform | ✓ | 逆映射逐像素反解 + 4×4 子采样覆盖；无旋转缩放时走中心采样快路径（与整数盒逐位一致）；斜向边缘为锯齿（无 AA，记录） |
//! | Text | ✓ 近似 | 最小 TrueType（cmap4/glyf 简单+复合字形）折线扫描线 16 级覆盖；基线 = Chromium 同法（hhea 取整 + 半行距）；无 kerning/GSUB、无合成粗斜体、span 覆盖忽略、仅 Start 对齐、max_advance 不折行（记录）；text-shadow blur>0=字形遮罩真模糊（装饰线不投影，Chromium 同语义）、blur=0=平移重发 |
//!
//! 字节零副作用：字体由宿主经 [`FontBank`] 提供（族名 → TTF 字节）；
//! `render` 不带字体库时跳过 Text（v0 行为）。

// 阶段3 API 冻结：公共项文档强制（C3 契约）。
#![deny(missing_docs)]

pub mod filter;
mod ttf;

use style_engine::css::property::{
    BlendMode, BorderStyle, ColorStop, FamilyName, FontFamilyList, GradientKind,
};
use style_engine::css::value::{ColorValue, LengthPercentage};
use style_engine::paint::{ConicGeom, RadialGeom};
use style_engine::{DisplayList, PaintOp};

/// 纯软件画布：RGBA8 直 alpha、sRGB 编码值（与 DisplayList 色彩语义一致）。
pub struct SoftCanvas {
    /// 画布宽度 px。
    pub width: u32,
    /// 画布高度 px。
    pub height: u32,
    /// 行主序 RGBA8，`pixels.len() == width * height * 4`。
    pub pixels: Vec<u8>,
}

/// 宿主字体库（零副作用：TTF 字节由宿主提供；族名 → 字节）。
/// Text op 的 `font_family` 按声明顺序取首个命中库的 Named 族。
#[derive(Default)]
pub struct FontBank {
    entries: Vec<(String, Vec<u8>)>,
}

impl FontBank {
    /// 创建空字体库。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册字体：family 名 + 完整字体文件字节（重复 family 后注册者优先）。
    pub fn add(&mut self, family: &str, data: Vec<u8>) {
        self.entries.push((family.to_string(), data));
    }

    /// 按 family 名取字体文件字节；未注册返回 None。
    pub fn get(&self, family: &str) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|(name, _)| name == family)
            .map(|(_, data)| data.as_slice())
    }

    /// 是否未注册任何字体。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 2D 仿射 [a, b, c, d, e, f]（列向量：x' = a·x + c·y + e）。
#[derive(Clone, Copy, Debug)]
struct Mat {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Mat {
    fn identity() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    fn from_affine(m: &[f32; 6]) -> Self {
        Self {
            a: m[0],
            b: m[1],
            c: m[2],
            d: m[3],
            e: m[4],
            f: m[5],
        }
    }

    /// self ∘ n（先应用 n）——与 paint.rs mul_affine 同块乘。
    fn mul(self, n: Self) -> Self {
        Self {
            a: self.a * n.a + self.c * n.b,
            b: self.b * n.a + self.d * n.b,
            c: self.a * n.c + self.c * n.d,
            d: self.b * n.c + self.d * n.d,
            e: self.a * n.e + self.c * n.f + self.e,
            f: self.b * n.e + self.d * n.f + self.f,
        }
    }

    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    fn invert(&self) -> Option<Self> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 {
            return None;
        }
        Some(Self {
            a: self.d / det,
            b: -self.b / det,
            c: -self.c / det,
            d: self.a / det,
            e: (self.c * self.f - self.d * self.e) / det,
            f: (self.b * self.e - self.a * self.f) / det,
        })
    }
}

/// 多边形裁剪（F3c，ADR-0025）：nonzero winding / evenodd 射线法内含测试；
/// 设备→源空间映射同 ClipRect（push 时刻矩阵之逆；奇异 = 恒不可见）。
struct ClipPoly {
    pts: Vec<(f32, f32)>,
    nonzero: bool,
    inv: Option<Mat>,
}

/// 活跃裁剪项（PushClip 矩形 / PushClipPath 多边形；单一 LIFO 栈）。
enum Clip {
    /// 矩形+圆角。
    Rect(ClipRect),
    /// 多边形。
    Poly(ClipPoly),
}

struct ClipRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: [f32; 8],
    /// 设备→裁剪源空间映射（push 时刻矩阵之逆；奇异 = 恒不可见）。
    inv: Option<Mat>,
}

struct OpacityLayer {
    snapshot: Vec<u8>,
    alpha: f32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// 混合层（P1-2，css-compositing-1）：push 时刻快照底图，pop 时刻按
/// [`BlendMode`] 将层内容与底图逐像素合成。全 18 种模式原生实现
/// （含 vello Mix 枚举没有的 plus-lighter/plus-darker）。
struct BlendLayer {
    snapshot: Vec<u8>,
    mode: BlendMode,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

// ===== 混合模式数学（css-compositing-1 §4；直排 sRGB RGBA，分量 [0,1]）=====

/// 逐通道可分离混合函数 B(Cb,Cs)。
fn blend_separable(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::ColorDodge => {
            if cb <= 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        BlendMode::HardLight => {
            if cs <= 0.5 {
                blend_separable(BlendMode::Multiply, cb, 2.0 * cs)
            } else {
                blend_separable(BlendMode::Screen, cb, 2.0 * cs - 1.0)
            }
        }
        // Overlay(Cb,Cs) = HardLight(Cs,Cb)
        BlendMode::Overlay => {
            if cb <= 0.5 {
                blend_separable(BlendMode::Multiply, cs, 2.0 * cb)
            } else {
                blend_separable(BlendMode::Screen, cs, 2.0 * cb - 1.0)
            }
        }
        BlendMode::SoftLight => {
            // D(x)（W3C 分段定义）
            let d = |x: f32| {
                if x <= 0.25 {
                    ((16.0 * x - 12.0) * x + 4.0) * x
                } else {
                    x.sqrt()
                }
            };
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                cb + (2.0 * cs - 1.0) * (d(cb) - cb)
            }
        }
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::Exclusion => cb + cs - 2.0 * cb * cs,
        _ => cs, // 不可达：调用方保证可分离模式分派
    }
}

/// 亮度（W3C 系数）。
fn blend_lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

/// 夹取 RGB 到单位立方（沿亮度线投影）。
fn blend_clip_color(mut c: [f32; 3]) -> [f32; 3] {
    let l = blend_lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    if n < 0.0 {
        for v in &mut c {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for v in &mut c {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    c
}

/// 设定亮度。
fn blend_set_lum(mut c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - blend_lum(c);
    for v in &mut c {
        *v += d;
    }
    blend_clip_color(c)
}

/// 饱和度 = max − min。
fn blend_sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

/// 设定饱和度（max/mid/min 通道重标定）。
fn blend_set_sat(mut c: [f32; 3], s: f32) -> [f32; 3] {
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| c[a].total_cmp(&c[b]));
    let (mn, md, mx) = (idx[0], idx[1], idx[2]);
    if c[mx] > c[mn] {
        c[md] = (c[md] - c[mn]) * s / (c[mx] - c[mn]);
        c[mx] = s;
    } else {
        c[md] = 0.0;
        c[mx] = 0.0;
    }
    c[mn] = 0.0;
    c
}

/// 非可分离混合函数 B(Cb,Cs)（Hue/Saturation/Color/Luminosity）。
fn blend_non_separable(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    match mode {
        BlendMode::Hue => blend_set_lum(blend_set_sat(cs, blend_sat(cb)), blend_lum(cb)),
        BlendMode::Saturation => blend_set_lum(blend_set_sat(cb, blend_sat(cs)), blend_lum(cb)),
        BlendMode::Color => blend_set_lum(cs, blend_lum(cb)),
        BlendMode::Luminosity => blend_set_lum(cb, blend_lum(cs)),
        _ => cs, // 不可达：调用方保证非可分离模式分派
    }
}

/// 单像素混合合成（css-compositing-1 §5.1 全式；直排 RGBA，返回 4 分量）。
fn blend_pixel(mode: BlendMode, back: [f32; 4], src: [f32; 4]) -> [f32; 4] {
    let (cb, ab) = (back, back[3]);
    let (cs, a_s) = (src, src[3]);
    let ao = (a_s + ab * (1.0 - a_s)).clamp(0.0, 1.0);
    if ao <= 0.0 {
        return [0.0; 4];
    }
    match mode {
        // 加法族按预乘定义（PDF/CG 惯例；css-compositing-2 附录），
        // 合成后 straight 化。
        BlendMode::PlusLighter | BlendMode::PlusDarker => {
            let mut out = [0.0f32; 4];
            for c in 0..3 {
                let premul = cb[c] * ab + cs[c] * a_s;
                out[c] = match mode {
                    BlendMode::PlusDarker => (premul - 1.0).max(0.0),
                    _ => premul.clamp(0.0, 1.0),
                };
            }
            out[3] = ao;
            for v in &mut out[..3] {
                *v /= ao;
            }
            out
        }
        BlendMode::Normal => {
            // B = Cs → 全式退化为 src-over（直排）：αs·Cs + (1−αs)·αb·Cb
            let mut out = [0.0f32; 4];
            for c in 0..3 {
                out[c] = a_s * cs[c] + ab * (1.0 - a_s) * cb[c];
            }
            out[3] = ao;
            out
        }
        BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => {
            let b = blend_non_separable(mode, [cb[0], cb[1], cb[2]], [cs[0], cs[1], cs[2]]);
            let mut out = [0.0f32; 4];
            for c in 0..3 {
                out[c] = a_s * (1.0 - ab) * cs[c] + a_s * ab * b[c] + ab * (1.0 - a_s) * cb[c];
            }
            out[3] = ao;
            out
        }
        mode => {
            let mut out = [0.0f32; 4];
            for c in 0..3 {
                let b = blend_separable(mode, cb[c], cs[c]);
                out[c] = a_s * (1.0 - ab) * cs[c] + a_s * ab * b + ab * (1.0 - a_s) * cb[c];
            }
            out[3] = ao;
            out
        }
    }
}

/// 4×4 子采样偏移（像素内 16 点网格中心）。
const SUBS: [f32; 4] = [0.125, 0.375, 0.625, 0.875];

/// 将绘制清单光栅化到 `width×height` 画布（`base` 为初始底色；无字体库，
/// Text op 跳过——与 v0 行为一致）。
pub fn render(list: &DisplayList, width: u32, height: u32, base: [u8; 4]) -> SoftCanvas {
    render_with_fonts(list, width, height, base, &FontBank::new())
}

/// 光栅化（带宿主字体库）：Text op 经 [`FontBank`] 取字形。
pub fn render_with_fonts(
    list: &DisplayList,
    width: u32,
    height: u32,
    base: [u8; 4],
    bank: &FontBank,
) -> SoftCanvas {
    let mut canvas = SoftCanvas {
        width,
        height,
        pixels: vec![0u8; width as usize * height as usize * 4],
    };
    for px in canvas.pixels.as_chunks_mut::<4>().0 {
        px.copy_from_slice(&base);
    }
    let mut clips: Vec<Clip> = Vec::new();
    let mut mat = Mat::identity();
    let mut mat_stack: Vec<Mat> = Vec::new();
    let mut layers: Vec<OpacityLayer> = Vec::new();
    let mut blends: Vec<BlendLayer> = Vec::new();
    for op in &list.ops {
        apply_op(
            op,
            &mut canvas,
            &mut clips,
            &mut mat,
            &mut mat_stack,
            &mut layers,
            &mut blends,
            bank,
        );
    }
    canvas
}

#[allow(clippy::too_many_arguments)]
fn apply_op(
    op: &PaintOp,
    canvas: &mut SoftCanvas,
    clips: &mut Vec<Clip>,
    mat: &mut Mat,
    mat_stack: &mut Vec<Mat>,
    layers: &mut Vec<OpacityLayer>,
    blends: &mut Vec<BlendLayer>,
    bank: &FontBank,
) {
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
                *mat,
                *x,
                *y,
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
            conic,
            linear,
        } => {
            let (geom, line) = match gradient.kind {
                GradientKind::Linear(angle) => {
                    // CSS 角度：0deg=向上，顺时针；方向向量 = (sin θ, -cos θ)；
                    // 源空间采样（scroll/transform 已折叠进矩阵）。
                    let rad = angle.0.to_radians();
                    let (sx, sy) = (rad.sin(), -rad.cos());
                    let len = (width * sx).abs() + (height * sy).abs();
                    (None, (sx, sy, len.max(1e-6)))
                }
                GradientKind::Radial(_) => {
                    // paint 层已解析绝对几何（RadialGeom，源空间）；渐变线 = 水平半径
                    let g = radial.unwrap_or(RadialGeom {
                        cx: x + width / 2.0,
                        cy: y + height / 2.0,
                        rx: width.max(1.0) / 2.0,
                        ry: height.max(1.0) / 2.0,
                    });
                    (Some(g), (0.0, 0.0, g.rx.max(1e-6)))
                }
                // 阶段3 API 冻结：GradientKind 未来变体按缺省径向几何降级（非穷举演进契约）。
                _ => {
                    let g = radial.unwrap_or(RadialGeom {
                        cx: x + width / 2.0,
                        cy: y + height / 2.0,
                        rx: width.max(1.0) / 2.0,
                        ry: height.max(1.0) / 2.0,
                    });
                    (Some(g), (0.0, 0.0, g.rx.max(1e-6)))
                }
            };
            let (bx, by, bw, bh) = (*x, *y, *width, *height);
            // 锥形（C3，ADR-0017 D2）：圆心/起角 paint 层已解析（源空间）；
            // 缺省几何 = 盒心 + 12 点方向起（−π/2）。
            let cone = match gradient.kind {
                GradientKind::Conic(_) => Some(conic.unwrap_or(ConicGeom {
                    cx: x + width / 2.0,
                    cy: y + height / 2.0,
                    start: -(std::f32::consts::FRAC_PI_2),
                })),
                _ => None,
            };
            let stops = gradient.stops.clone();
            // F3d（ADR-0026）：linear 绝对几何优先——采样 = 对渐变线段
            // （全盒解析，9-slice 区域共用）的归一投影；否则退回盒心投影。
            let lin = *linear;
            let line_len = lin
                .map(|g| ((g.end[0] - g.start[0]).powi(2) + (g.end[1] - g.start[1]).powi(2)).sqrt())
                .unwrap_or(line.2);
            // repeating（css-images-3，P1-3）：停点模式周期 = 首末停点跨距
            // （全线索引分数）；采样 t 取模回周期内再插值。周期 0（显式停点
            // 逆序抬升后首末重合等）→ 透明黑（source-over 之下 = 无操作）。
            let repeating = if gradient.repeating {
                let pos = stop_positions(&gradient.stops, line_len);
                let first = pos.first().copied().unwrap_or(0.0);
                let period = pos.last().copied().unwrap_or(0.0) - first;
                Some((first, period))
            } else {
                None
            };
            fill_rect(
                canvas,
                clips,
                *mat,
                bx,
                by,
                bw,
                bh,
                radius,
                move |fx, fy| {
                    let t = if let Some(gm) = cone {
                        // 锥形（C3）：扫角归一——atan2 自 +X 轴、Y-down 顺时针
                        //（与 CSS/peniko Sweep 同向）；rel = ang − start 归一到 [0, 2π)。
                        let ang = (fy - gm.cy).atan2(fx - gm.cx);
                        let rel = ang - gm.start;
                        rel.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU
                    } else {
                        match geom {
                            // 线性：绝对渐变线段投影（F3d）优先，退回盒心投影
                            None => {
                                if let Some(gm) = lin {
                                    let dx = gm.end[0] - gm.start[0];
                                    let dy = gm.end[1] - gm.start[1];
                                    ((fx - gm.start[0]) * dx + (fy - gm.start[1]) * dy)
                                        / (dx * dx + dy * dy).max(1e-9)
                                } else {
                                    let (sx, sy, len) = line;
                                    let (cx, cy) = (bx + bw / 2.0, by + bh / 2.0);
                                    ((fx - cx) * sx + (fy - cy) * sy) / len + 0.5
                                }
                            }
                            // 径向：椭圆归一距离
                            Some(g) => {
                                let nx = (fx - g.cx) / g.rx;
                                let ny = (fy - g.cy) / g.ry;
                                (nx * nx + ny * ny).sqrt()
                            }
                        }
                    };
                    if let Some((first, period)) = repeating {
                        if !(period.is_finite() && period > 1e-6) {
                            return [0.0, 0.0, 0.0, 0.0];
                        }
                        // t 可越出 [0,1]（CSS repeating 沿轴无限平铺）：
                        // u = first + mod(t − first, period) 落回首末停点间
                        let u = first + (t - first).rem_euclid(period);
                        return stop_at(&stops, u.clamp(0.0, 1.0), line_len);
                    }
                    stop_at(&stops, t.clamp(0.0, 1.0), line_len)
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
            blur,
            ..
        } => {
            let c = components_of_color(color.components);
            // P1c 真 blur：blur>0 且矩阵纯平移 → 形状 alpha 遮罩 + 3×盒
            // 模糊（σ=blur/2，见 draw_blurred_shadow）；其余（旋转/缩放
            // 矩阵）保持平移矩形近似（记录偏差）。
            let fast = mat.b.abs() < 1e-6
                && mat.c.abs() < 1e-6
                && (mat.a - 1.0).abs() < 1e-6
                && (mat.d - 1.0).abs() < 1e-6;
            if *blur > 0.0 && fast {
                draw_blurred_shadow(
                    canvas, clips, *mat, *x, *y, *width, *height, radius, c, *offset_x, *offset_y,
                    *spread, *inset, *blur,
                );
            } else {
                // blur=0 原路径：外阴影=外扩平移矩形；内阴影=平移矩形
                // 与盒体求交。
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
                fill_rect(canvas, clips, *mat, sx, sy, sw, sh, radius, move |_, _| c);
            }
        }
        PaintOp::Image {
            x,
            y,
            width,
            height,
            radius,
            pixels: img,
            src_x,
            src_y,
            src_w,
            src_h,
            ..
        } => {
            if img.width == 0 || img.height == 0 {
                return;
            }
            let (ix, iy, iw, ih) = (*x, *y, *width, *height);
            // F3d 9-slice（ADR-0026）：采样窗 = src 子域（全图 = 0/0/源尺寸，
            // 行为不变）；dest 盒 → 子域线性映射 → 源位图最近邻。
            let (sxf, syf, swf, shf) = (*src_x, *src_y, *src_w, *src_h);
            let img = img.clone();
            fill_rect(
                canvas,
                clips,
                *mat,
                ix,
                iy,
                iw,
                ih,
                radius,
                move |fx, fy| {
                    // 最近邻采样（源空间坐标 → 源位图；经 src 子域偏移缩放）
                    let u = (sxf + (fx - ix) / iw.max(1e-6) * swf).clamp(0.0, 0.999_9);
                    let v = (syf + (fy - iy) / ih.max(1e-6) * shf).clamp(0.0, 0.999_9);
                    let sx = (u * img.width as f32) as usize;
                    let sy = (v * img.height as f32) as usize;
                    let bytes = img.rgba.as_ref().as_ref();
                    let si = (sy * img.width as usize + sx) * 4;
                    [
                        bytes[si] as f32 / 255.0,
                        bytes[si + 1] as f32 / 255.0,
                        bytes[si + 2] as f32 / 255.0,
                        bytes[si + 3] as f32 / 255.0,
                    ]
                },
            );
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
                    *mat,
                    bx,
                    by,
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
        } => {
            clips.push(Clip::Rect(ClipRect {
                x: *x,
                y: *y,
                w: *width,
                h: *height,
                radius: *radius,
                inv: mat.invert(),
            }));
        }
        PaintOp::PopClip => {
            clips.pop();
        }
        PaintOp::PushClipPath { points, nonzero } => {
            // F3c（ADR-0025）：多边形裁剪——顶点视口坐标（源空间）直存，
            // 内含测试经矩阵逆映射逐点判定（nonzero/evenodd 射线法）。
            clips.push(Clip::Poly(ClipPoly {
                pts: points.iter().map(|p| (p[0], p[1])).collect(),
                nonzero: *nonzero,
                inv: mat.invert(),
            }));
        }
        PaintOp::PushOpacity {
            alpha,
            x,
            y,
            width,
            height,
        } => {
            // 混合区域 = op 盒经当前矩阵的设备包围盒
            let corners = [
                mat.apply(*x, *y),
                mat.apply(x + width, *y),
                mat.apply(x + width, y + height),
                mat.apply(*x, y + height),
            ];
            let min_x = corners.iter().fold(f32::MAX, |m, p| m.min(p.0));
            let min_y = corners.iter().fold(f32::MAX, |m, p| m.min(p.1));
            let max_x = corners.iter().fold(f32::MIN, |m, p| m.max(p.0));
            let max_y = corners.iter().fold(f32::MIN, |m, p| m.max(p.1));
            let bx = (min_x.max(0.0).floor() as u32).min(canvas.width);
            let by = (min_y.max(0.0).floor() as u32).min(canvas.height);
            let ex = (max_x.min(canvas.width as f32).ceil() as u32).min(canvas.width);
            let ey = (max_y.min(canvas.height as f32).ceil() as u32).min(canvas.height);
            layers.push(OpacityLayer {
                snapshot: canvas.pixels.clone(),
                alpha: alpha.clamp(0.0, 1.0),
                x: bx,
                y: by,
                w: ex.saturating_sub(bx),
                h: ey.saturating_sub(by),
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
        PaintOp::PushBlend {
            mode,
            x,
            y,
            width,
            height,
        } => {
            // 混合区域 = op 盒经当前矩阵的设备包围盒（同 PushOpacity 约定）。
            // push 时快照底图并**清空区域**：子树内容先累积在透明底上，
            // pop 时整体与快照按 mode 合成——这保证混合语义以整组内容
            // 与背后画布发生，而非逐笔画叠加。
            let corners = [
                mat.apply(*x, *y),
                mat.apply(x + width, *y),
                mat.apply(x + width, y + height),
                mat.apply(*x, y + height),
            ];
            let min_x = corners.iter().fold(f32::MAX, |m, p| m.min(p.0));
            let min_y = corners.iter().fold(f32::MAX, |m, p| m.min(p.1));
            let max_x = corners.iter().fold(f32::MIN, |m, p| m.max(p.0));
            let max_y = corners.iter().fold(f32::MIN, |m, p| m.max(p.1));
            let bx = (min_x.max(0.0).floor() as u32).min(canvas.width);
            let by = (min_y.max(0.0).floor() as u32).min(canvas.height);
            let ex = (max_x.min(canvas.width as f32).ceil() as u32).min(canvas.width);
            let ey = (max_y.min(canvas.height as f32).ceil() as u32).min(canvas.height);
            blends.push(BlendLayer {
                snapshot: canvas.pixels.clone(),
                mode: *mode,
                x: bx,
                y: by,
                w: ex.saturating_sub(bx),
                h: ey.saturating_sub(by),
            });
            let x1 = ex.min(canvas.width);
            let y1 = ey.min(canvas.height);
            for py in by..y1 {
                let row = py as usize * canvas.width as usize;
                for px in bx..x1 {
                    canvas.pixels[(row + px as usize) * 4..(row + px as usize) * 4 + 4].fill(0);
                }
            }
        }
        PaintOp::PopBlend => {
            if let Some(layer) = blends.pop() {
                let x1 = (layer.x + layer.w).min(canvas.width);
                let y1 = (layer.y + layer.h).min(canvas.height);
                for py in layer.y..y1 {
                    for px in layer.x..x1 {
                        let i = (py as usize * canvas.width as usize + px as usize) * 4;
                        let back = [
                            layer.snapshot[i] as f32 / 255.0,
                            layer.snapshot[i + 1] as f32 / 255.0,
                            layer.snapshot[i + 2] as f32 / 255.0,
                            layer.snapshot[i + 3] as f32 / 255.0,
                        ];
                        let src = [
                            canvas.pixels[i] as f32 / 255.0,
                            canvas.pixels[i + 1] as f32 / 255.0,
                            canvas.pixels[i + 2] as f32 / 255.0,
                            canvas.pixels[i + 3] as f32 / 255.0,
                        ];
                        let out = blend_pixel(layer.mode, back, src);
                        for (c, v) in out.iter().enumerate() {
                            canvas.pixels[i + c] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
                        }
                    }
                }
            }
        }
        PaintOp::PushScroll { dx, dy } => {
            mat_stack.push(*mat);
            *mat = Mat {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                e: *dx,
                f: *dy,
            }
            .mul(*mat);
        }
        PaintOp::PopScroll => {
            if let Some(prev) = mat_stack.pop() {
                *mat = prev;
            }
        }
        PaintOp::PushTransform { affine } => {
            mat_stack.push(*mat);
            // 变换作用于子树局部坐标：设备 = cur(M(局部)) → cur∘M
            *mat = mat.mul(Mat::from_affine(affine));
        }
        PaintOp::PopTransform => {
            if let Some(prev) = mat_stack.pop() {
                *mat = prev;
            }
        }
        PaintOp::Text {
            x,
            y,
            text,
            color,
            font_size,
            font_family,
            letter_spacing,
            line_height,
            text_align,
            decorations,
            shadows,
            font_stretch,
            word_spacing,
            ..
        } => {
            // v1 仅 Start 对齐（非 Start 按 Start 绘制——记录偏差）；
            // span 覆盖/合成粗斜体/max_advance 折行同属记录边界。
            // F3d：features/variations 无消费点（FontBank 无 GSUB/gvar，
            // 记录偏差）；stretch/word-spacing 伪合成消费（见 draw_text）。
            let _ = text_align;
            // F2（ADR-0022 D5）：影字先绘。P1c：blur>0 → 字形遮罩真模糊
            // （装饰线不投影——Chromium 同语义）；blur=0 → 原平移重发
            // （含装饰线，保持既有像素输出）。
            for s in shadows.iter() {
                if s.blur > 0.0 {
                    draw_text_shadow_blur(
                        canvas,
                        clips,
                        *mat,
                        *x + s.dx,
                        *y + s.dy,
                        text,
                        s.color.components,
                        *font_size,
                        font_family,
                        *letter_spacing,
                        *line_height,
                        *font_stretch,
                        *word_spacing,
                        bank,
                        s.blur,
                    );
                } else {
                    draw_text(
                        canvas,
                        clips,
                        *mat,
                        *x + s.dx,
                        *y + s.dy,
                        text,
                        s.color.components,
                        *font_size,
                        font_family,
                        *letter_spacing,
                        *line_height,
                        *font_stretch,
                        *word_spacing,
                        bank,
                        decorations,
                    );
                }
            }
            draw_text(
                canvas,
                clips,
                *mat,
                *x,
                *y,
                text,
                color.components,
                *font_size,
                font_family,
                *letter_spacing,
                *line_height,
                *font_stretch,
                *word_spacing,
                bank,
                decorations,
            );
        }
        // 未识别 op 忽略（PaintOp 非穷举演进——契约 sink 对未来 op 的默认
        // 语义=忽略）。
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

/// 源空间矩形内含测试（含圆角）。
fn src_inside(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: &[f32; 8]) -> bool {
    px >= x && px < x + w && py >= y && py < y + h && corner_ok(px, py, x, y, w, h, r)
}

/// 源空间多边形内含测试（F3c ADR-0025；nonzero = 环数 ≠ 0，evenodd =
/// 射线穿越奇偶；半开边规则 yi ≤ py < yj 仅计上穿，顶点重合不双计；
/// <3 顶点 = 空区域恒不可见）。
fn poly_inside(px: f32, py: f32, pts: &[(f32, f32)], nonzero: bool) -> bool {
    let n = pts.len();
    if n < 3 {
        return false;
    }
    let mut winding = 0i32;
    let mut crossed = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = pts[i];
        let (xj, yj) = pts[j];
        // 有向边 i→j 相对点 P 的左侧量（>0 = P 在边左侧）
        let side = (xj - xi) * (py - yi) - (px - xi) * (yj - yi);
        if yi <= py {
            if yj > py && side > 0.0 {
                winding += 1;
                crossed = !crossed;
            }
        } else if yj <= py && side < 0.0 {
            winding -= 1;
            crossed = !crossed;
        }
        j = i;
    }
    if nonzero { winding != 0 } else { crossed }
}

fn clip_ok(px: f32, py: f32, c: &Clip) -> bool {
    match c {
        Clip::Rect(r) => {
            let Some(inv) = &r.inv else { return false };
            let (sx, sy) = inv.apply(px, py);
            src_inside(sx, sy, r.x, r.y, r.w, r.h, &r.radius)
        }
        Clip::Poly(p) => {
            let Some(inv) = &p.inv else { return false };
            let (sx, sy) = inv.apply(px, py);
            poly_inside(sx, sy, &p.pts, p.nonzero)
        }
    }
}

fn clips_ok(clips: &[Clip], px: f32, py: f32) -> bool {
    clips.iter().all(|c| clip_ok(px, py, c))
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

/// 矩形区域逐像素填充（源空间几何 + 矩阵逆映射到设备；像素光栅器签名
/// 天然多参——圆角/裁剪/取色闭包）。无旋转缩放（a=d=1, b=c=0）走单中心
/// 采样快路径（与整数盒逐位一致）；否则 4×4 子采样累积覆盖做边缘近似。
#[allow(clippy::too_many_arguments)]
fn fill_rect(
    canvas: &mut SoftCanvas,
    clips: &[Clip],
    mat: Mat,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: &[f32; 8],
    color_at: impl Fn(f32, f32) -> [f32; 4],
) {
    let Some(inv) = mat.invert() else { return };
    // 设备 bbox = 四角变换后的包围盒（与画布求交）
    let corners = [
        mat.apply(x, y),
        mat.apply(x + w, y),
        mat.apply(x + w, y + h),
        mat.apply(x, y + h),
    ];
    let min_x = corners.iter().fold(f32::MAX, |m, p| m.min(p.0));
    let min_y = corners.iter().fold(f32::MAX, |m, p| m.min(p.1));
    let max_x = corners.iter().fold(f32::MIN, |m, p| m.max(p.0));
    let max_y = corners.iter().fold(f32::MIN, |m, p| m.max(p.1));
    let x0 = min_x.floor().max(0.0) as i64;
    let y0 = min_y.floor().max(0.0) as i64;
    let x1 = (max_x.ceil() as i64).min(canvas.width as i64);
    let y1 = (max_y.ceil() as i64).min(canvas.height as i64);
    let fast = mat.b.abs() < 1e-6
        && mat.c.abs() < 1e-6
        && (mat.a - 1.0).abs() < 1e-6
        && (mat.d - 1.0).abs() < 1e-6;
    for py in y0..y1 {
        for px in x0..x1 {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            if fast {
                let (sx, sy) = inv.apply(fx, fy);
                if !src_inside(sx, sy, x, y, w, h, radius) {
                    continue;
                }
                if !clips_ok(clips, fx, fy) {
                    continue;
                }
                let c = color_at(sx, sy);
                let i = (py as usize * canvas.width as usize + px as usize) * 4;
                blend(&mut canvas.pixels[i..i + 4], c);
            } else {
                let mut hits = 0u32;
                for &oy in &SUBS {
                    for &ox in &SUBS {
                        let (ssx, ssy) = inv.apply(px as f32 + ox, py as f32 + oy);
                        if !src_inside(ssx, ssy, x, y, w, h, radius) {
                            continue;
                        }
                        if !clips_ok(clips, px as f32 + ox, py as f32 + oy) {
                            continue;
                        }
                        hits += 1;
                    }
                }
                if hits == 0 {
                    continue;
                }
                let (sx, sy) = inv.apply(fx, fy);
                let mut c = color_at(sx, sy);
                c[3] *= hits as f32 / 16.0;
                let i = (py as usize * canvas.width as usize + px as usize) * 4;
                blend(&mut canvas.pixels[i..i + 4], c);
            }
        }
    }
}

/// 字形折线扫描线填充（设备空间折线 + 4×4 子行 16 级覆盖；非零环绕）。
fn fill_polygons(
    canvas: &mut SoftCanvas,
    clips: &[Clip],
    polys: &[Vec<(f32, f32)>],
    color: [f32; 4],
) {
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for poly in polys {
        for &(x, y) in poly {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if min_x > max_x || min_y > max_y {
        return;
    }
    let x0 = min_x.floor().max(0.0) as i64;
    let y0 = min_y.floor().max(0.0) as i64;
    let x1 = ((max_x + 1.0).ceil() as i64).min(canvas.width as i64);
    let y1 = ((max_y + 1.0).ceil() as i64).min(canvas.height as i64);
    let mut edges: Vec<(f32, f32, f32, f32)> = Vec::new();
    for poly in polys {
        let n = poly.len();
        if n < 3 {
            continue;
        }
        for i in 0..n {
            let a = poly[i];
            let b = poly[(i + 1) % n];
            if a.1 != b.1 {
                edges.push((a.0, a.1, b.0, b.1));
            }
        }
    }
    if edges.is_empty() {
        return;
    }
    let mut cov = vec![0.0f32; (x1 - x0).max(0) as usize];
    for py in y0..y1 {
        for c in cov.iter_mut() {
            *c = 0.0;
        }
        for s in 0..4 {
            let ys = py as f32 + (s as f32 + 0.5) * 0.25;
            let mut xs: Vec<(f32, i32)> = Vec::new();
            for &(ex0, ey0, ex1, ey1) in &edges {
                let (lo, hi) = if ey0 < ey1 { (ey0, ey1) } else { (ey1, ey0) };
                if ys < lo || ys >= hi {
                    continue;
                }
                let t = (ys - ey0) / (ey1 - ey0);
                xs.push((ex0 + t * (ex1 - ex0), if ey1 > ey0 { 1 } else { -1 }));
            }
            if xs.is_empty() {
                continue;
            }
            xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let mut winding = 0i32;
            let mut span_start = 0.0f32;
            for (x, d) in xs {
                if winding != 0 {
                    add_span(&mut cov, x0 as f32, span_start, x);
                }
                winding += d;
                span_start = x;
            }
        }
        for (i, &c) in cov.iter().enumerate() {
            if c <= 0.0 {
                continue;
            }
            let px = x0 + i as i64;
            if !clips_ok(clips, px as f32 + 0.5, py as f32 + 0.5) {
                continue;
            }
            let idx = (py as usize * canvas.width as usize + px as usize) * 4;
            blend(
                &mut canvas.pixels[idx..idx + 4],
                [color[0], color[1], color[2], color[3] * c.min(1.0)],
            );
        }
    }
}

/// 半开区间 [xa, xb) 的覆盖累加（每子行权重 0.25）。
fn add_span(cov: &mut [f32], base_x: f32, xa: f32, xb: f32) {
    if xb <= xa {
        return;
    }
    let first = xa.floor() as i64;
    let last = (xb - 1e-4).floor() as i64;
    for px in first..=last {
        let left = (px as f32).max(xa);
        let right = ((px + 1) as f32).min(xb);
        if right > left {
            let idx = px - base_x as i64;
            if idx >= 0 && (idx as usize) < cov.len() {
                cov[idx as usize] += (right - left) * 0.25;
            }
        }
    }
}

// ===== P1c：真 blur 基建（形状 alpha 遮罩 + 3×可分离盒模糊）=====

/// 字形组：每字符一组轮廓（保持逐字符 fill_polygons 粒度）。
type GlyphGroups = Vec<Vec<Vec<(f32, f32)>>>;
/// 装饰线组：附色矩形折线（填充序=原实现）。
type DecoGroups = Vec<([f32; 4], Vec<(f32, f32)>)>;
/// 遮罩合成的额外钳位域（inset=盒矩形 x/y/w/h + 圆角）。
type MaskBound = (f32, f32, f32, f32, [f32; 8]);

/// 三遍盒模糊的盒宽（逼近高斯 σ：单遍均匀盒方差 (w²−1)/12，三遍合计
/// (w²−1)/4 = σ² → w=√(4σ²+1)，取奇数、下限 1）。
pub(crate) fn box_width_for_sigma(sigma: f32) -> usize {
    if sigma <= 0.0 {
        return 1;
    }
    let w = (4.0 * sigma * sigma + 1.0).sqrt().round() as i64;
    (w.max(1) as usize) | 1
}

/// u8 alpha 遮罩 3×可分离盒模糊（水平/垂直交替三遍；逐像素窗口 u32
/// 累加、`(sum + len/2)/len` 取整——全程整数运算、固定遍历序，逐位
/// 确定）。边界=钳位延拓（遮罩 pad=⌈3σ⌉，边缘邻域值≈0，钳位影响
/// 可忽略）。
pub(crate) fn blur_alpha_u8(mask: &mut [u8], mw: usize, mh: usize, sigma: f32) {
    let bw = box_width_for_sigma(sigma);
    if bw <= 1 || mw == 0 || mh == 0 {
        return;
    }
    let half = (bw / 2) as i64;
    let len = bw as u32;
    let rnd = len / 2;
    let mut tmp = vec![0u8; mw * mh];
    let mut tmp2 = vec![0u8; mw * mh];
    for _ in 0..3 {
        // 水平：mask → tmp
        for y in 0..mh {
            let row = &mask[y * mw..(y + 1) * mw];
            let at = |i: i64| row[i.clamp(0, mw as i64 - 1) as usize] as u32;
            for x in 0..mw {
                let xi = x as i64;
                let sum = (xi - half..=xi + half).map(at).sum::<u32>();
                tmp[y * mw + x] = ((sum + rnd) / len) as u8;
            }
        }
        // 垂直：tmp → tmp2
        for x in 0..mw {
            let at = |i: i64| tmp[i.clamp(0, mh as i64 - 1) as usize * mw + x] as u32;
            for y in 0..mh {
                let yi = y as i64;
                let sum = (yi - half..=yi + half).map(at).sum::<u32>();
                tmp2[y * mw + x] = ((sum + rnd) / len) as u8;
            }
        }
        mask.copy_from_slice(&tmp2);
    }
}

/// 设备空间圆角矩形 → u8 遮罩（像素中心采样；`clear=true` 时矩形内
/// 清零——inset 内影的"洞"，否则矩形内置 255）。
#[allow(clippy::too_many_arguments)]
fn mask_rect(
    mask: &mut [u8],
    mw: usize,
    mh: usize,
    ox: i64,
    oy: i64,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: &[f32; 8],
    clear: bool,
) {
    for iy in 0..mh {
        let py = (oy + iy as i64) as f32 + 0.5;
        for ix in 0..mw {
            let px = (ox + ix as i64) as f32 + 0.5;
            if src_inside(px, py, x, y, w, h, r) {
                let idx = iy * mw + ix;
                mask[idx] = if clear { 0 } else { 255 };
            }
        }
    }
}

/// 字形折线 → u8 遮罩（与 [`fill_polygons`] 同扫描线算法：4×4 子行
/// 16 级覆盖、nonzero 环绕；写入取 max——重叠字形不叠加）。区域以
/// 遮罩缓冲为界（设备坐标 (ox, oy) 起，尺寸 mw×mh）。
fn fill_polygons_mask(
    mask: &mut [u8],
    mw: i64,
    mh: i64,
    ox: i64,
    oy: i64,
    polys: &[Vec<(f32, f32)>],
) {
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for poly in polys {
        for &(x, y) in poly {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if min_x > max_x || min_y > max_y {
        return;
    }
    let x0 = (min_x.floor() as i64).max(ox);
    let y0 = (min_y.floor() as i64).max(oy);
    let x1 = (((max_x + 1.0).ceil() as i64).min(ox + mw)).max(x0);
    let y1 = (((max_y + 1.0).ceil() as i64).min(oy + mh)).max(y0);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let mut edges: Vec<(f32, f32, f32, f32)> = Vec::new();
    for poly in polys {
        let n = poly.len();
        if n < 3 {
            continue;
        }
        for i in 0..n {
            let a = poly[i];
            let b = poly[(i + 1) % n];
            if a.1 != b.1 {
                edges.push((a.0, a.1, b.0, b.1));
            }
        }
    }
    if edges.is_empty() {
        return;
    }
    let mut cov = vec![0.0f32; (x1 - x0).max(0) as usize];
    for py in y0..y1 {
        for c in cov.iter_mut() {
            *c = 0.0;
        }
        for s in 0..4 {
            let ys = py as f32 + (s as f32 + 0.5) * 0.25;
            let mut xs: Vec<(f32, i32)> = Vec::new();
            for &(ex0, ey0, ex1, ey1) in &edges {
                let (lo, hi) = if ey0 < ey1 { (ey0, ey1) } else { (ey1, ey0) };
                if ys < lo || ys >= hi {
                    continue;
                }
                let t = (ys - ey0) / (ey1 - ey0);
                xs.push((ex0 + t * (ex1 - ex0), if ey1 > ey0 { 1 } else { -1 }));
            }
            if xs.is_empty() {
                continue;
            }
            xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let mut winding = 0i32;
            let mut span_start = 0.0f32;
            for (x, d) in xs {
                if winding != 0 {
                    add_span(&mut cov, x0 as f32, span_start, x);
                }
                winding += d;
                span_start = x;
            }
        }
        for (i, &c) in cov.iter().enumerate() {
            if c <= 0.0 {
                continue;
            }
            let mx = x0 + i as i64 - ox;
            let my = py - oy;
            let v = (c.min(1.0) * 255.0).round() as u8;
            let idx = (my * mw + mx) as usize;
            mask[idx] = mask[idx].max(v);
        }
    }
}

/// 遮罩着色合成：`alpha = color.a × mask/255`，逐像素 src-over；clips
/// 之外或 `bound`（inset=盒矩形，含圆角）之外跳过。
#[allow(clippy::too_many_arguments)]
fn composite_mask(
    canvas: &mut SoftCanvas,
    clips: &[Clip],
    mask: &[u8],
    mw: i64,
    ox: i64,
    oy: i64,
    color: [f32; 4],
    bound: Option<MaskBound>,
) {
    if mw <= 0 {
        return;
    }
    let mh = mask.len() as i64 / mw;
    for my in 0..mh {
        for mx in 0..mw {
            let a = mask[(my * mw + mx) as usize];
            if a == 0 {
                continue;
            }
            let dx = (ox + mx) as f32 + 0.5;
            let dy = (oy + my) as f32 + 0.5;
            if !clips_ok(clips, dx, dy) {
                continue;
            }
            if let Some((bx, by, bw, bh, br)) = bound
                && !src_inside(dx, dy, bx, by, bw, bh, &br)
            {
                continue;
            }
            let af = (a as f32 / 255.0) * color[3];
            let i = ((oy + my) as usize * canvas.width as usize + (ox + mx) as usize) * 4;
            blend(
                &mut canvas.pixels[i..i + 4],
                [color[0], color[1], color[2], af],
            );
        }
    }
}

/// 真模糊盒阴影（P1c）：设备空间形状 alpha 遮罩 + [`blur_alpha_u8`]
/// （σ=blur/2）+ [`composite_mask`]。outset=外扩 spread 的圆角矩形
/// （圆角随 spread 增缩、钳半宽防退化椭圆）；inset=盒内减平移扩展
/// 矩形（合成期钳回盒内）。遮罩区域=形状盒 ± ⌈3σ⌉ ∩ 画布。仅纯
/// 平移矩阵调用（旋转/缩放回退平移矩形近似——模块表记录偏差）。
#[allow(clippy::too_many_arguments)]
fn draw_blurred_shadow(
    canvas: &mut SoftCanvas,
    clips: &[Clip],
    mat: Mat,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: &[f32; 8],
    color: [f32; 4],
    ox: f32,
    oy: f32,
    spread: f32,
    inset: bool,
    blur: f32,
) {
    let sigma = (blur * 0.5).max(0.25);
    let pad = (3.0 * sigma).ceil() as i64;
    // 纯平移矩阵：设备矩形 = 源矩形 + (e, f)。
    let (dx, dy) = (x + mat.e, y + mat.f);
    let radii = |rw: f32, rh: f32, grow: f32| -> [f32; 8] {
        let hx = (rw * 0.5).max(0.0);
        let hy = (rh * 0.5).max(0.0);
        [
            (radius[0] + grow).max(0.0).min(hx),
            (radius[1] + grow).max(0.0).min(hy),
            (radius[2] + grow).max(0.0).min(hx),
            (radius[3] + grow).max(0.0).min(hy),
            (radius[4] + grow).max(0.0).min(hx),
            (radius[5] + grow).max(0.0).min(hy),
            (radius[6] + grow).max(0.0).min(hx),
            (radius[7] + grow).max(0.0).min(hy),
        ]
    };
    let (sx, sy, sw, sh) = if inset {
        (dx, dy, w, h)
    } else {
        (
            dx + ox - spread,
            dy + oy - spread,
            w + 2.0 * spread,
            h + 2.0 * spread,
        )
    };
    if sw <= 0.0 || sh <= 0.0 {
        return;
    }
    let mx0 = ((sx.floor() as i64) - pad).max(0);
    let my0 = ((sy.floor() as i64) - pad).max(0);
    let mx1 = ((sx + sw).ceil() as i64 + pad).min(canvas.width as i64);
    let my1 = ((sy + sh).ceil() as i64 + pad).min(canvas.height as i64);
    if mx0 >= mx1 || my0 >= my1 {
        return;
    }
    let (mw, mh) = ((mx1 - mx0) as usize, (my1 - my0) as usize);
    let mut mask = vec![0u8; mw * mh];
    if inset {
        mask_rect(
            &mut mask,
            mw,
            mh,
            mx0,
            my0,
            dx,
            dy,
            w,
            h,
            &radii(w, h, 0.0),
            false,
        );
        let (hx, hy, hw, hh) = (
            dx + ox - spread,
            dy + oy - spread,
            w + 2.0 * spread,
            h + 2.0 * spread,
        );
        if hw > 0.0 && hh > 0.0 {
            mask_rect(
                &mut mask,
                mw,
                mh,
                mx0,
                my0,
                hx,
                hy,
                hw,
                hh,
                &radii(hw, hh, spread),
                true,
            );
        }
    } else {
        mask_rect(
            &mut mask,
            mw,
            mh,
            mx0,
            my0,
            sx,
            sy,
            sw,
            sh,
            &radii(sw, sh, spread),
            false,
        );
    }
    blur_alpha_u8(&mut mask, mw, mh, sigma);
    let bound = if inset {
        Some((dx, dy, w, h, radii(w, h, 0.0)))
    } else {
        None
    };
    composite_mask(canvas, clips, &mask, mw as i64, mx0, my0, color, bound);
}

/// text-shadow blur>0：字形折线 → 遮罩（[`fill_polygons_mask`]）→
/// [`blur_alpha_u8`]（σ=blur/2）→ 着色合成。装饰线不投影（Chromium
/// 同语义）；offset 已由调用方计入 x/y。
#[allow(clippy::too_many_arguments)]
fn draw_text_shadow_blur(
    canvas: &mut SoftCanvas,
    clips: &[Clip],
    mat: Mat,
    x: f32,
    y: f32,
    text: &str,
    color: [f32; 4],
    font_size: f32,
    family: &FontFamilyList,
    letter_spacing: f32,
    line_height: Option<f32>,
    font_stretch: f32,
    word_spacing: Option<f32>,
    bank: &FontBank,
    blur: f32,
) {
    let (glyphs, _) = text_device_polys(
        mat,
        x,
        y,
        text,
        font_size,
        family,
        letter_spacing,
        line_height,
        font_stretch,
        word_spacing,
        bank,
        &[],
    );
    let flat: Vec<Vec<(f32, f32)>> = glyphs.into_iter().flatten().collect();
    if flat.is_empty() {
        return;
    }
    let sigma = (blur * 0.5).max(0.25);
    let pad = (3.0 * sigma).ceil() as i64;
    let mut min_x = f32::MAX;
    let mut min_y = f32::MAX;
    let mut max_x = f32::MIN;
    let mut max_y = f32::MIN;
    for poly in &flat {
        for &(px, py) in poly {
            min_x = min_x.min(px);
            min_y = min_y.min(py);
            max_x = max_x.max(px);
            max_y = max_y.max(py);
        }
    }
    let mx0 = ((min_x.floor() as i64) - pad).max(0);
    let my0 = ((min_y.floor() as i64) - pad).max(0);
    let mx1 = ((max_x.ceil() as i64) + pad).min(canvas.width as i64);
    let my1 = ((max_y.ceil() as i64) + pad).min(canvas.height as i64);
    if mx0 >= mx1 || my0 >= my1 {
        return;
    }
    let (mw, mh) = ((mx1 - mx0) as usize, (my1 - my0) as usize);
    let mut mask = vec![0u8; mw * mh];
    fill_polygons_mask(&mut mask, mw as i64, mh as i64, mx0, my0, &flat);
    blur_alpha_u8(&mut mask, mw, mh, sigma);
    composite_mask(canvas, clips, &mask, mw as i64, mx0, my0, color, None);
}

/// Text 设备空间折线提取：字形（每字符一组轮廓——保持原逐字符
/// fill_polygons 粒度，重叠字形逐次 src-over）+ 装饰线矩形（附色，
/// 填充序与原实现一致）。字体未命中 → 空组。供 [`draw_text`] 与
/// [`draw_text_shadow_blur`] 共用（P1c 重构，光栅输出逐位不变）。
#[allow(clippy::too_many_arguments)]
fn text_device_polys(
    mat: Mat,
    x: f32,
    y: f32,
    text: &str,
    font_size: f32,
    family: &FontFamilyList,
    letter_spacing: f32,
    line_height: Option<f32>,
    font_stretch: f32,
    word_spacing: Option<f32>,
    bank: &FontBank,
    decorations: &[style_engine::paint::TextDecorationPaint],
) -> (GlyphGroups, DecoGroups) {
    let mut glyph_groups: GlyphGroups = Vec::new();
    let mut deco_groups: DecoGroups = Vec::new();
    let Some(data) = family.0.iter().find_map(|f| match f {
        FamilyName::Named(name) => bank.get(name),
        _ => None,
    }) else {
        return (glyph_groups, deco_groups); // 未命中字体 → 跳过该 Text（记录偏差）
    };
    let Some(font) = ttf::SoftFont::parse(data) else {
        return (glyph_groups, deco_groups);
    };
    let scale = font.scale_for(font_size);
    let asc = font.ascender as f32 * scale;
    let desc = -font.descender as f32 * scale;
    let (asc_i, desc_i) = (asc.round(), desc.round());
    let lh = line_height.unwrap_or(asc_i + desc_i);
    let baseline = y + (lh - (asc_i + desc_i)) * 0.5 + asc_i;
    // F3d：伪 font-stretch（x 向同比；100 = 恒等）+ word-spacing 空格追加。
    let fw = (font_stretch / 100.0).max(0.01);
    let ws = word_spacing.unwrap_or(0.0);
    let mut pen = x;
    for ch in text.chars() {
        // 未映射码点 → notdef（gid 0，DejaVu 为盒形——与 Chromium 同语义）
        let gid = font.lookup(ch).unwrap_or(0);
        let polys_font = font.outline(gid);
        if !polys_font.is_empty() {
            let mut contours: Vec<Vec<(f32, f32)>> = Vec::with_capacity(polys_font.len());
            for cont in &polys_font {
                let mut dp = Vec::with_capacity(cont.len());
                for &(fx, fy) in cont {
                    let (sx, sy) = (pen + fx * scale * fw, baseline - fy * scale);
                    dp.push(mat.apply(sx, sy));
                }
                contours.push(dp);
            }
            glyph_groups.push(contours);
        }
        pen += font.advance(gid, scale) * fw + letter_spacing;
        if ch == ' ' {
            pen += ws;
        }
    }
    // F2（ADR-0022 D4）：装饰线（软栅格=矩形多边形填充；单行语义——
    // 软 sink 不折行（记录边界），行几何=asc/desc 基线系；线位与 vello
    // 同式：underline=baseline+desc*0.5、overline=baseline−asc*0.9、
    // line-through=baseline−asc*0.5，B 级近似在案）。
    if !decorations.is_empty() {
        let end_x = pen; // 字形循环后 pen=总 advance 终点
        for d in decorations {
            let t = d.thickness_px.max(0.5);
            let lines = [
                (d.line & 1 != 0, baseline + desc * 0.5),
                (d.line & 2 != 0, baseline - asc * 0.9),
                (d.line & 4 != 0, baseline - asc * 0.5),
            ];
            for (on, cy) in lines {
                if !on {
                    continue;
                }
                let rect = vec![
                    mat.apply(x, cy - t * 0.5),
                    mat.apply(end_x, cy - t * 0.5),
                    mat.apply(end_x, cy + t * 0.5),
                    mat.apply(x, cy + t * 0.5),
                ];
                deco_groups.push((d.color.components, rect));
            }
        }
    }
    (glyph_groups, deco_groups)
}

/// Text op 光栅化：家族命中 [`FontBank`] → 最小 TrueType → 折线 → 扫描线。
/// 基线与 Chromium 同法：hhea asc/desc 取整，行盒内半行距居中；
/// normal（None）= round(asc)+round(desc)（与引擎 ㉔ 同式）。
/// F3d（ADR-0026 D5）：font-stretch 伪合成（字形轮廓与步进同比 x 向
/// 缩放 fw=stretch/100——DejaVu 无 width 轴，B 级在案）；
/// word-spacing = 每空格字形后追加像素；features/variations 无消费点。
#[allow(clippy::too_many_arguments)]
fn draw_text(
    canvas: &mut SoftCanvas,
    clips: &[Clip],
    mat: Mat,
    x: f32,
    y: f32,
    text: &str,
    color: [f32; 4],
    font_size: f32,
    family: &FontFamilyList,
    letter_spacing: f32,
    line_height: Option<f32>,
    font_stretch: f32,
    word_spacing: Option<f32>,
    bank: &FontBank,
    decorations: &[style_engine::paint::TextDecorationPaint],
) {
    let (glyph_groups, deco_groups) = text_device_polys(
        mat,
        x,
        y,
        text,
        font_size,
        family,
        letter_spacing,
        line_height,
        font_stretch,
        word_spacing,
        bank,
        decorations,
    );
    for contours in &glyph_groups {
        fill_polygons(canvas, clips, contours, color);
    }
    for (dc, rect) in &deco_groups {
        fill_polygons(canvas, clips, std::slice::from_ref(rect), *dc);
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
    // css-images-3 §4.5.2：显式位置逆序时抬至前停位（单调化；
    // 解析器不夹取，用值期语义归 sink）
    for i in 1..n {
        if pos[i] < pos[i - 1] {
            pos[i] = pos[i - 1];
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

#[cfg(test)]
mod tests {
    use super::*;
    use peniko::color::{AlphaColor, Srgb};
    use style_engine::css::property::Gradient;
    use style_engine::css::value::Angle;
    use style_engine::paint::DisplayList;
    use style_engine::smallvec::smallvec;

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

    fn pixel(c: &SoftCanvas, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * c.width + x) * 4) as usize;
        [c.pixels[i], c.pixels[i + 1], c.pixels[i + 2]]
    }

    fn op_text(x: f32, y: f32, text: &str) -> PaintOp {
        PaintOp::Text {
            x,
            y,
            text: text.to_string(),
            color: rgba([0.0, 0.0, 0.0, 1.0]),
            spans: Vec::new(),
            font_size: 16.0,
            font_family: FontFamilyList(smallvec![FamilyName::Named("DejaVu Sans".into())]),
            font_weight: 400.0,
            italic: false,
            max_advance: None,
            line_height: Some(19.0),
            letter_spacing: 0.0,
            text_align: style_engine::css::property::TextAlign::Start,
            word_break: style_engine::css::property::WordBreakKind::Normal,
            overflow_wrap: style_engine::css::property::OverflowWrapKind::Normal,
            decorations: Vec::new(),
            shadows: Vec::new(),
            font_stretch: 100.0,
            word_spacing: None,
            font_features: Vec::new(),
            font_variations: Vec::new(),
        }
    }

    /// DejaVu Sans（demo 资产，与 conformance 用例同源字节）。
    const TEST_FONT: &[u8] = include_bytes!("../../style-engine-demo/assets/fonts/DejaVuSans.ttf");

    fn font_bank() -> FontBank {
        let mut b = FontBank::new();
        b.add("DejaVu Sans", TEST_FONT.to_vec());
        b
    }

    /// 墨迹包围盒 (min_x, min_y, max_x, max_y)（亮度 < 128 视为墨迹）。
    fn ink_bbox(c: &SoftCanvas) -> Option<(u32, u32, u32, u32)> {
        let mut r: Option<(u32, u32, u32, u32)> = None;
        for y in 0..c.height {
            for x in 0..c.width {
                let p = pixel(c, x, y);
                if p[0] < 128 && p[1] < 128 && p[2] < 128 {
                    r = Some(match r {
                        None => (x, y, x, y),
                        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                    });
                }
            }
        }
        r
    }

    // ===== P1c：真 blur =====

    #[allow(clippy::too_many_arguments)]
    fn op_shadow(
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: [f32; 4],
        dx: f32,
        dy: f32,
        spread: f32,
        blur: f32,
        inset: bool,
        radius: [f32; 8],
    ) -> PaintOp {
        PaintOp::Shadow {
            x,
            y,
            width: w,
            height: h,
            radius,
            color: rgba(color),
            offset_x: dx,
            offset_y: dy,
            spread,
            blur,
            inset,
        }
    }

    fn gray_at(c: &SoftCanvas, x: u32, y: u32) -> u8 {
        pixel(c, x, y)[0]
    }

    #[test]
    fn box_shadow_blur_softens_edge_and_deterministic() {
        // blur=8（σ=4）：盒内全影、盒缘外单调衰减、pad=3σ 之外为零；
        // 两次渲染逐字节一致（整数滑动窗确定性）。
        let mut list = DisplayList::default();
        list.ops.push(op_shadow(
            16.0,
            16.0,
            16.0,
            16.0,
            [0.0, 0.0, 0.0, 1.0],
            0.0,
            0.0,
            0.0,
            8.0,
            false,
            [0.0; 8],
        ));
        let c = render(&list, 48, 48, [255, 255, 255, 255]);
        assert!(gray_at(&c, 24, 24) < 60, "center fully shadowed");
        assert!(
            gray_at(&c, 24, 15) < gray_at(&c, 24, 12),
            "monotonic falloff outward"
        );
        assert!(gray_at(&c, 24, 12) < 255, "bleed outside box edge");
        assert_eq!(gray_at(&c, 2, 2), 255, "beyond 3σ pad untouched");
        let c2 = render(&list, 48, 48, [255, 255, 255, 255]);
        assert_eq!(c.pixels, c2.pixels, "byte-deterministic");
    }

    #[test]
    fn box_shadow_blur_rounded_spread_shape() {
        // spread=4 外扩 + 圆角：形状盒 12..36，(2,2) 距形状 > pad=6 → 白。
        let mut list = DisplayList::default();
        list.ops.push(op_shadow(
            16.0,
            16.0,
            16.0,
            16.0,
            [0.0, 0.0, 0.0, 1.0],
            0.0,
            0.0,
            4.0,
            4.0,
            false,
            [4.0; 8],
        ));
        let c = render(&list, 48, 48, [255, 255, 255, 255]);
        assert!(gray_at(&c, 24, 24) < 60, "center dark");
        assert_eq!(gray_at(&c, 2, 2), 255, "outside pad white");
    }

    #[test]
    fn inset_shadow_blur_clipped_to_box() {
        // inset：影只见于盒内（上缘暗、洞心白）；盒外即使模糊可达也钳回。
        let mut list = DisplayList::default();
        list.ops.push(op_shadow(
            12.0,
            12.0,
            24.0,
            24.0,
            [0.0, 0.0, 0.0, 1.0],
            0.0,
            4.0,
            0.0,
            6.0,
            true,
            [0.0; 8],
        ));
        let c = render(&list, 48, 48, [255, 255, 255, 255]);
        assert!(gray_at(&c, 24, 13) < 200, "top edge dark");
        assert!(gray_at(&c, 24, 28) >= 250, "hole interior clean");
        assert_eq!(gray_at(&c, 24, 5), 255, "outside box clipped");
    }

    #[test]
    fn text_shadow_blur_glyph_mask_deterministic() {
        use style_engine::paint::TextShadowPaint;
        let mut op = op_text(8.0, 8.0, "Hi");
        if let PaintOp::Text { shadows, .. } = &mut op {
            *shadows = vec![TextShadowPaint {
                dx: 1.0,
                dy: 1.0,
                blur: 3.0,
                color: rgba([1.0, 0.0, 0.0, 1.0]),
            }];
        }
        let mut list = DisplayList::default();
        list.ops.push(op);
        let c = render_with_fonts(&list, 64, 32, [255, 255, 255, 255], &font_bank());
        // 红影存在（r 显著高于 g/b）且两次渲染逐字节一致。
        let reds = (0..c.width)
            .flat_map(|x| (0..c.height).map(move |y| (x, y)))
            .filter(|&(x, y)| {
                let p = pixel(&c, x, y);
                p[0] as i32 - p[1].max(p[2]) as i32 > 40
            })
            .count();
        assert!(reds > 20, "blurred red shadow visible, reds={reds}");
        let c2 = render_with_fonts(&list, 64, 32, [255, 255, 255, 255], &font_bank());
        assert_eq!(c.pixels, c2.pixels, "byte-deterministic");
    }

    #[test]
    fn text_shadow_blur_zero_keeps_legacy_path() {
        // blur=0 与「平移重发两个 Text op」逐字节一致（原路径未动）。
        use style_engine::paint::TextShadowPaint;
        let mut op = op_text(8.0, 8.0, "Hi");
        if let PaintOp::Text { shadows, .. } = &mut op {
            *shadows = vec![TextShadowPaint {
                dx: 2.0,
                dy: 3.0,
                blur: 0.0,
                color: rgba([0.0, 0.0, 1.0, 1.0]),
            }];
        }
        let mut list = DisplayList::default();
        list.ops.push(op);
        let c = render_with_fonts(&list, 64, 32, [255, 255, 255, 255], &font_bank());
        let mut legacy = DisplayList::default();
        // legacy 影字 op 颜色须与被测 shadow 同色（op_text 默认黑）。
        let mut shadow_op = op_text(10.0, 11.0, "Hi");
        if let PaintOp::Text { color, .. } = &mut shadow_op {
            *color = rgba([0.0, 0.0, 1.0, 1.0]);
        }
        legacy.ops.push(shadow_op);
        legacy.ops.push(op_text(8.0, 8.0, "Hi"));
        let c2 = render_with_fonts(&legacy, 64, 32, [255, 255, 255, 255], &font_bank());
        assert_eq!(c.pixels, c2.pixels, "blur=0 == shifted re-emit");
    }

    #[test]
    fn box_shadow_blur_rotated_matrix_fallback() {
        // 非平移矩阵回退平移矩形近似（记录偏差）：不 panic、有墨迹。
        let mut list = DisplayList::default();
        let rad = std::f32::consts::FRAC_PI_4;
        list.ops.push(PaintOp::PushTransform {
            affine: [rad.cos(), rad.sin(), -rad.sin(), rad.cos(), 0.0, 0.0],
        });
        list.ops.push(op_shadow(
            16.0,
            16.0,
            16.0,
            16.0,
            [0.0, 0.0, 0.0, 1.0],
            0.0,
            0.0,
            0.0,
            8.0,
            false,
            [0.0; 8],
        ));
        list.ops.push(PaintOp::PopTransform);
        let c = render(&list, 64, 64, [255, 255, 255, 255]);
        let ink = (0..c.width)
            .flat_map(|x| (0..c.height).map(move |y| (x, y)))
            .filter(|&(x, y)| gray_at(&c, x, y) < 250)
            .count();
        assert!(ink > 0, "fallback shadow still paints, ink={ink}");
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
    fn conic_quadrant_colors() {
        // C3（ADR-0017 D5）：0deg（12 点）起、四象限硬停点——扫角几何像素锁。
        use style_engine::css::property::ConicSpec;
        use style_engine::paint::ConicGeom;
        let red = ColorValue::Absolute(rgba([1.0, 0.0, 0.0, 1.0]));
        let blue = ColorValue::Absolute(rgba([0.0, 0.0, 1.0, 1.0]));
        let p = |v: f32| Some(LengthPercentage::Percent(v));
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::Gradient {
            x: 0.0,
            y: 0.0,
            width: 8.0,
            height: 8.0,
            radius: [0.0; 8],
            gradient: Gradient {
                repeating: false,
                kind: GradientKind::Conic(ConicSpec {
                    from: Angle(0.0),
                    position: (
                        LengthPercentage::Percent(0.5),
                        LengthPercentage::Percent(0.5),
                    ),
                }),
                stops: vec![
                    ColorStop {
                        color: red.clone(),
                        position: p(0.0),
                    },
                    ColorStop {
                        color: red.clone(),
                        position: p(0.25),
                    },
                    ColorStop {
                        color: blue.clone(),
                        position: p(0.25),
                    },
                    ColorStop {
                        color: blue.clone(),
                        position: p(0.5),
                    },
                    ColorStop {
                        color: red.clone(),
                        position: p(0.5),
                    },
                    ColorStop {
                        color: red.clone(),
                        position: p(0.75),
                    },
                    ColorStop {
                        color: blue.clone(),
                        position: p(0.75),
                    },
                    ColorStop {
                        color: blue.clone(),
                        position: p(1.0),
                    },
                ],
            },
            radial: None,
            conic: Some(ConicGeom {
                cx: 4.0,
                cy: 4.0,
                start: -(std::f32::consts::FRAC_PI_2),
            }),
            linear: None,
        });
        let c = render(&list, 8, 8, [0, 0, 0, 255]);
        let px = |x: usize, y: usize| &c.pixels[(y * 8 + x) * 4..(y * 8 + x) * 4 + 3];
        // 盒心 (4,4)、start=−π/2：45°→t=0.125 红；135°→0.375 蓝；
        // 225°→0.625 红；315°→0.875 蓝（Y-down 顺时针 = CSS 同向）。
        assert_eq!(px(5, 2), &[255, 0, 0], "右上 45° 应为红");
        assert_eq!(px(5, 5), &[0, 0, 255], "右下 135° 应为蓝");
        assert_eq!(px(2, 5), &[255, 0, 0], "左下 225° 应为红");
        assert_eq!(px(2, 2), &[0, 0, 255], "左上 315° 应为蓝");
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
                repeating: false,
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
            conic: None,
            linear: None,
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
    fn repeating_linear_gradient_tiles_period() {
        // P1-3（css-images-3）：40px 盒 repeating-linear-gradient(90deg,
        // red 0px, blue 20px) → 周期 20px（线长分数 0.5），沿轴无限平铺；
        // 像素中心采样 t=fx/40，u=mod(t,0.5)，k=u/0.5。
        let grad = |repeating: bool| {
            let mut list = DisplayList::default();
            list.ops.push(PaintOp::Gradient {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 2.0,
                radius: [0.0; 8],
                gradient: Gradient {
                    repeating,
                    kind: GradientKind::Linear(Angle(90.0)),
                    stops: vec![
                        ColorStop {
                            color: ColorValue::Absolute(rgba([1.0, 0.0, 0.0, 1.0])),
                            position: Some(LengthPercentage::Px(0.0)),
                        },
                        ColorStop {
                            color: ColorValue::Absolute(rgba([0.0, 0.0, 1.0, 1.0])),
                            position: Some(LengthPercentage::Px(20.0)),
                        },
                    ],
                },
                radial: None,
                conic: None,
                linear: None,
            });
            list
        };
        let c = render(&grad(true), 40, 2, [255, 255, 255, 255]);
        let at = |x: usize| {
            let i = x * 4;
            [&c.pixels[i], &c.pixels[i + 1], &c.pixels[i + 2]]
        };
        // 手算锁值：x=0（中心 0.5px）k=0.025 → (249,0,6)；
        // x=5（中心 5.5px）k=0.275 → (185,0,70)；x=10 k=0.525 → (121,0,134)。
        assert_eq!(at(0), [&249, &0, &6]);
        assert_eq!(at(5), [&185, &0, &70]);
        assert_eq!(at(10), [&121, &0, &134]);
        // 周期性：x 与 x+20 同色（模式沿轴平铺）
        assert_eq!(at(5), at(25), "周期 20px：x=5 与 x=25 应同色");
        assert_eq!(at(10), at(30), "周期 20px：x=10 与 x=30 应同色");
        // 非 repeating 对照：t≥0.5 全取末色 blue（超出末停即 clamp）
        let c2 = render(&grad(false), 40, 2, [255, 255, 255, 255]);
        let i = 30 * 4;
        assert_eq!(
            [&c2.pixels[i], &c2.pixels[i + 1], &c2.pixels[i + 2]],
            [&0, &0, &255],
            "非 repeating 右半应整体取末停 blue"
        );
    }

    #[test]
    fn repeating_gradient_zero_period_transparent() {
        // 周期 0：red 20px / blue 20px（首末停点重合）→ 透明黑，画布不变。
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::Gradient {
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 2.0,
            radius: [0.0; 8],
            gradient: Gradient {
                repeating: true,
                kind: GradientKind::Linear(Angle(90.0)),
                stops: vec![
                    ColorStop {
                        color: ColorValue::Absolute(rgba([1.0, 0.0, 0.0, 1.0])),
                        position: Some(LengthPercentage::Px(20.0)),
                    },
                    ColorStop {
                        color: ColorValue::Absolute(rgba([0.0, 0.0, 1.0, 1.0])),
                        position: Some(LengthPercentage::Px(20.0)),
                    },
                ],
            },
            radial: None,
            conic: None,
            linear: None,
        });
        let c = render(&list, 40, 2, [255, 255, 255, 255]);
        assert_eq!(&c.pixels[0..4], &[255, 255, 255, 255], "周期 0 应无操作");
        let i = 20 * 4;
        assert_eq!(&c.pixels[i..i + 4], &[255, 255, 255, 255]);
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

    #[test]
    fn scroll_translation_folds() {
        // PushScroll 平移折叠：矩形整体位移（⑦：scroll 并入矩阵语义等价）
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::PushScroll { dx: 2.0, dy: 1.0 });
        list.ops
            .push(op_fill(0.0, 0.0, 2.0, 2.0, [0.0; 8], [1.0, 0.0, 0.0, 1.0]));
        list.ops.push(PaintOp::PopScroll);
        let c = render(&list, 8, 8, [255, 255, 255, 255]);
        assert_eq!(pixel(&c, 2, 1), [255, 0, 0]);
        assert_eq!(pixel(&c, 3, 2), [255, 0, 0]);
        assert_eq!(pixel(&c, 0, 0), [255, 255, 255]);
    }

    #[test]
    fn transform_translate_fill() {
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::PushTransform {
            affine: [1.0, 0.0, 0.0, 1.0, 2.0, 1.0],
        });
        list.ops
            .push(op_fill(1.0, 1.0, 2.0, 2.0, [0.0; 8], [1.0, 0.0, 0.0, 1.0]));
        list.ops.push(PaintOp::PopTransform);
        let c = render(&list, 8, 8, [255, 255, 255, 255]);
        // (1,1,2,2) 平移 (2,1) → 设备 (3..5, 2..4)
        assert_eq!(pixel(&c, 3, 2), [255, 0, 0]);
        assert_eq!(pixel(&c, 4, 3), [255, 0, 0]);
        assert_eq!(pixel(&c, 2, 2), [255, 255, 255]);
        assert_eq!(pixel(&c, 5, 3), [255, 255, 255]);
    }

    #[test]
    fn transform_rotate_90_crisp() {
        // 顺时针 90°（y-down）：(x,y) → (4−y, x)；矩形 (0,0,4,2) →
        // 设备 x∈[2,4), y∈[0,2)。轴对齐整数边 → 无 AA、二值覆盖。
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::PushTransform {
            affine: [0.0, 1.0, -1.0, 0.0, 4.0, 0.0],
        });
        list.ops
            .push(op_fill(0.0, 0.0, 4.0, 2.0, [0.0; 8], [1.0, 0.0, 0.0, 1.0]));
        list.ops.push(PaintOp::PopTransform);
        let c = render(&list, 8, 8, [255, 255, 255, 255]);
        for x in 2..4 {
            for y in 0..2 {
                assert_eq!(pixel(&c, x, y), [255, 0, 0], "({x},{y}) 应为红");
            }
        }
        assert_eq!(pixel(&c, 1, 0), [255, 255, 255]);
        assert_eq!(pixel(&c, 4, 1), [255, 255, 255]);
    }

    #[test]
    fn transform_scale2_fill() {
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::PushTransform {
            affine: [2.0, 0.0, 0.0, 2.0, 0.0, 0.0],
        });
        list.ops
            .push(op_fill(1.0, 1.0, 2.0, 2.0, [0.0; 8], [1.0, 0.0, 0.0, 1.0]));
        list.ops.push(PaintOp::PopTransform);
        let c = render(&list, 8, 8, [255, 255, 255, 255]);
        // (1,1,2,2) ×2 → 设备 (2..6, 2..6)
        assert_eq!(pixel(&c, 2, 2), [255, 0, 0]);
        assert_eq!(pixel(&c, 5, 5), [255, 0, 0]);
        assert_eq!(pixel(&c, 1, 2), [255, 255, 255]);
        assert_eq!(pixel(&c, 6, 5), [255, 255, 255]);
    }

    #[test]
    fn transform_clip_rotated() {
        // 裁剪矩节点随所在变换层：旋转空间裁剪 (0,0,4,1) → 设备 x∈[3,4)
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::PushTransform {
            affine: [0.0, 1.0, -1.0, 0.0, 4.0, 0.0],
        });
        list.ops.push(PaintOp::PushClip {
            x: 0.0,
            y: 0.0,
            width: 4.0,
            height: 1.0,
            radius: [0.0; 8],
        });
        list.ops
            .push(op_fill(0.0, 0.0, 4.0, 2.0, [0.0; 8], [1.0, 0.0, 0.0, 1.0]));
        list.ops.push(PaintOp::PopClip);
        list.ops.push(PaintOp::PopTransform);
        let c = render(&list, 8, 8, [255, 255, 255, 255]);
        assert_eq!(pixel(&c, 3, 0), [255, 0, 0]);
        assert_eq!(pixel(&c, 3, 1), [255, 0, 0]);
        assert_eq!(pixel(&c, 2, 0), [255, 255, 255]);
    }

    #[test]
    fn nested_transform_compose() {
        // 外层平移 + 内层旋转：矩阵复合 cur∘M
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::PushTransform {
            affine: [1.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        });
        list.ops.push(PaintOp::PushTransform {
            affine: [0.0, 1.0, -1.0, 0.0, 4.0, 0.0],
        });
        list.ops
            .push(op_fill(0.0, 0.0, 4.0, 2.0, [0.0; 8], [1.0, 0.0, 0.0, 1.0]));
        list.ops.push(PaintOp::PopTransform);
        list.ops.push(PaintOp::PopTransform);
        let c = render(&list, 8, 8, [255, 255, 255, 255]);
        // 旋转结果 (2..4, 0..2) 再平移 (1,1) → (3..5, 1..3)
        assert_eq!(pixel(&c, 3, 1), [255, 0, 0]);
        assert_eq!(pixel(&c, 4, 2), [255, 0, 0]);
        assert_eq!(pixel(&c, 2, 1), [255, 255, 255]);
    }

    #[test]
    fn text_glyph_ink() {
        // "A" 墨迹存在且落位合理：基线 = y + asc_i（DejaVu 16px asc≈14.85→15）
        let bank = font_bank();
        let mut list = DisplayList::default();
        list.ops.push(op_text(10.0, 12.0, "A"));
        let c = render_with_fonts(&list, 40, 40, [255, 255, 255, 255], &bank);
        let Some((x0, y0, x1, y1)) = ink_bbox(&c) else {
            panic!("应有墨迹");
        };
        assert!(x1 - x0 >= 6, "A 墨迹过窄: {x0}..{x1}");
        assert!(y1 - y0 >= 8, "A 墨迹过矮: {y0}..{y1}");
        // 大写 A 无降部：墨迹底应接近基线 12+15=27（±2）
        assert!((y1 as i32 - 27).abs() <= 2, "墨迹底 {y1} 应≈基线 27");
        // 墨迹顶 ≈ 基线 − 大写字高(~11.7) ≈ 15
        assert!((y0 as i32 - 15).abs() <= 2, "墨迹顶 {y0} 应≈15");
        // 无字体库 → 跳过（v0 行为）
        let c2 = render(&list, 40, 40, [255, 255, 255, 255]);
        assert!(ink_bbox(&c2).is_none());
    }

    #[test]
    fn text_composite_glyph_ink() {
        // "é"（U+00E9）：cmap4 → 复合字形（e + 重音符平移组件）递归展开
        let bank = font_bank();
        let mut list = DisplayList::default();
        list.ops.push(op_text(8.0, 12.0, "\u{00e9}"));
        let c = render_with_fonts(&list, 40, 40, [255, 255, 255, 255], &bank);
        let Some((_, y0, _, y1)) = ink_bbox(&c) else {
            panic!("复合字形应有墨迹");
        };
        // 重音符在 x 高之上：墨迹顶应明显高于基线 27 − x 高(~11)
        assert!(y0 < 16, "重音符墨迹顶 {y0} 应 <16");
        assert!(y1 >= 24, "e 主体墨迹底 {y1} 应≥24");
    }

    #[test]
    fn text_advance_layout() {
        // "aa" 第二字形按 hmtx 步进：整体墨迹宽 > 单 "a" + 4px
        let bank = font_bank();
        let single = {
            let mut list = DisplayList::default();
            list.ops.push(op_text(8.0, 12.0, "a"));
            render_with_fonts(&list, 48, 40, [255, 255, 255, 255], &bank)
        };
        let double = {
            let mut list = DisplayList::default();
            list.ops.push(op_text(8.0, 12.0, "aa"));
            render_with_fonts(&list, 48, 40, [255, 255, 255, 255], &bank)
        };
        let (_, _, ax1, _) = ink_bbox(&single).expect("a 墨迹");
        let (_, _, bx1, _) = ink_bbox(&double).expect("aa 墨迹");
        assert!(bx1 > ax1 + 4, "aa 末端 {bx1} 应显著大于 a 末端 {ax1}");
    }

    #[test]
    fn text_under_transform() {
        // 文本随变换层平移：与直接平移绘制等价（墨迹包围盒一致）
        let bank = font_bank();
        let shifted = {
            let mut list = DisplayList::default();
            list.ops.push(PaintOp::PushTransform {
                affine: [1.0, 0.0, 0.0, 1.0, 4.0, 0.0],
            });
            list.ops.push(op_text(10.0, 12.0, "A"));
            list.ops.push(PaintOp::PopTransform);
            render_with_fonts(&list, 48, 40, [255, 255, 255, 255], &bank)
        };
        let direct = {
            let mut list = DisplayList::default();
            list.ops.push(op_text(14.0, 12.0, "A"));
            render_with_fonts(&list, 48, 40, [255, 255, 255, 255], &bank)
        };
        assert_eq!(ink_bbox(&shifted), ink_bbox(&direct));
    }

    #[test]
    fn text_notdef_box() {
        // 未映射码点（私用区 U+E000）→ notdef 盒（与 Chromium 同语义）
        let bank = font_bank();
        let mut list = DisplayList::default();
        list.ops.push(op_text(8.0, 12.0, "\u{e000}"));
        let c = render_with_fonts(&list, 48, 40, [255, 255, 255, 255], &bank);
        assert!(ink_bbox(&c).is_some(), "notdef 应绘制盒形墨迹");
    }

    #[test]
    fn clip_path_polygon_pixels() {
        // F3c（ADR-0025）：多边形裁剪逐像素内含——三角内红/外白
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::PushClipPath {
            points: vec![[10.0, 10.0], [50.0, 10.0], [30.0, 50.0]],
            nonzero: true,
        });
        list.ops.push(op_fill(
            0.0,
            0.0,
            60.0,
            60.0,
            [0.0; 8],
            [1.0, 0.0, 0.0, 1.0],
        ));
        list.ops.push(PaintOp::PopClip);
        let c = render(&list, 60, 60, [255, 255, 255, 255]);
        let px = |x: usize, y: usize| -> [u8; 4] {
            let i = (y * 60 + x) * 4;
            [
                c.pixels[i],
                c.pixels[i + 1],
                c.pixels[i + 2],
                c.pixels[i + 3],
            ]
        };
        assert_eq!(px(30, 15), [255, 0, 0, 255], "三角内");
        assert_eq!(px(29, 45), [255, 0, 0, 255], "近下顶点内");
        assert_eq!(px(2, 2), [255, 255, 255, 255], "三角外(上)");
        assert_eq!(px(55, 55), [255, 255, 255, 255], "三角外(右下)");
        assert_eq!(px(2, 55), [255, 255, 255, 255], "三角外(左下)");
    }

    #[test]
    fn clip_path_fill_rule_pixels() {
        // F3c：{5/2} 五芒星序（0→2→4→1→3，真自交）——中心缠绕 2 → nonzero
        // 填充 / evenodd 镂空；角尖缠绕 1 → 两规则均填
        let pentagram = || {
            let mut pts = Vec::new();
            for k in 0..5usize {
                let v = (k * 2) % 5;
                let a = (v as f32) * std::f32::consts::TAU / 5.0 - std::f32::consts::FRAC_PI_2;
                pts.push([30.0 + 25.0 * a.cos(), 30.0 + 25.0 * a.sin()]);
            }
            pts
        };
        let render_with = |nonzero: bool| {
            let mut list = DisplayList::default();
            list.ops.push(PaintOp::PushClipPath {
                points: pentagram(),
                nonzero,
            });
            list.ops.push(op_fill(
                0.0,
                0.0,
                60.0,
                60.0,
                [0.0; 8],
                [1.0, 0.0, 0.0, 1.0],
            ));
            list.ops.push(PaintOp::PopClip);
            render(&list, 60, 60, [255, 255, 255, 255])
        };
        let px = |c: &SoftCanvas, x: usize, y: usize| -> [u8; 4] {
            let i = (y * 60 + x) * 4;
            [
                c.pixels[i],
                c.pixels[i + 1],
                c.pixels[i + 2],
                c.pixels[i + 3],
            ]
        };
        let nz = render_with(true);
        assert_eq!(px(&nz, 30, 30), [255, 0, 0, 255], "nonzero 中心填充");
        assert_eq!(px(&nz, 30, 10), [255, 0, 0, 255], "角臂两规则均填");
        let eo = render_with(false);
        assert_eq!(px(&eo, 30, 30), [255, 255, 255, 255], "evenodd 中心镂空");
        assert_eq!(px(&eo, 30, 10), [255, 0, 0, 255], "角臂两规则均填");
    }

    /// P1-2 测试辅助：全画布混合层 + 单个不透明填充，返回首像素 RGBA。
    fn blend_pixel_of(mode: BlendMode, base: [u8; 4], src: [f32; 4]) -> [u8; 4] {
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::PushBlend {
            mode,
            x: 0.0,
            y: 0.0,
            width: 4.0,
            height: 4.0,
        });
        list.ops.push(op_fill(0.0, 0.0, 4.0, 4.0, [0.0; 8], src));
        list.ops.push(PaintOp::PopBlend);
        let c = render(&list, 4, 4, base);
        let mut p = [0u8; 4];
        p.copy_from_slice(&c.pixels[0..4]);
        p
    }

    #[test]
    fn blend_modes_pixel_exact() {
        // P1-2：可分离/加法族手算精确值（css-compositing-1 §4；8bit 直排）。
        let red = [1.0, 0.0, 0.0, 1.0];
        let blue = [0.0, 0.0, 1.0, 1.0];
        let white = [255, 255, 255, 255];
        // multiply：cb·cs 逐通道 → 红底蓝源 = 乘黑 (0,0,0)
        assert_eq!(
            blend_pixel_of(BlendMode::Multiply, [255, 0, 0, 255], blue),
            [0, 0, 0, 255]
        );
        // screen：cb+cs−cb·cs → 红底蓝源 = (255,0,255)
        assert_eq!(
            blend_pixel_of(BlendMode::Screen, [255, 0, 0, 255], blue),
            [255, 0, 255, 255]
        );
        // difference：|cb−cs| → 白底蓝源 = (255,255,0)
        assert_eq!(
            blend_pixel_of(BlendMode::Difference, white, blue),
            [255, 255, 0, 255]
        );
        // darken / lighten：逐通道 min/max
        let dim = [100.0 / 255.0, 150.0 / 255.0, 60.0 / 255.0, 1.0];
        assert_eq!(
            blend_pixel_of(BlendMode::Darken, [200, 100, 50, 255], dim),
            [100, 100, 50, 255]
        );
        assert_eq!(
            blend_pixel_of(BlendMode::Lighten, [200, 100, 50, 255], dim),
            [200, 150, 60, 255]
        );
        // plus-lighter：预乘加法 → 红底蓝源 = (255,0,255)
        assert_eq!(
            blend_pixel_of(BlendMode::PlusLighter, [255, 0, 0, 255], blue),
            [255, 0, 255, 255]
        );
        // plus-darker：max(0, Db+Ds−1) → 白底蓝源 = (0,0,255)
        assert_eq!(
            blend_pixel_of(BlendMode::PlusDarker, white, blue),
            [0, 0, 255, 255]
        );
        // normal：αs=1 时 = 源（不透明覆盖）
        assert_eq!(
            blend_pixel_of(BlendMode::Normal, [255, 0, 0, 255], blue),
            [0, 0, 255, 255]
        );
    }

    #[test]
    fn blend_partial_alpha_and_non_separable() {
        // 半透明源 × multiply：白底 + 50% 红组（fill 写 u8 后 α=128/255）
        // → Co_R = αs·B_R(=αs) + (1−αs) = 0.25196+0.49804 = 0.75 → 191；
        //   Co_G/B = (1−αs) = 127
        let half_red = [1.0, 0.0, 0.0, 0.5];
        assert_eq!(
            blend_pixel_of(BlendMode::Multiply, [255, 255, 255, 255], half_red),
            [191, 127, 127, 255]
        );
        // 混合组内 opacity 嵌套（blend 外层包 opacity 内层）：
        // 白底 + blend(multiply) + opacity(0.5)(红) → opacity 出口像素
        // (128,0,0,128)，multiply 合成 → (191,127,127)
        let mut list = DisplayList::default();
        list.ops.push(PaintOp::PushBlend {
            mode: BlendMode::Multiply,
            x: 0.0,
            y: 0.0,
            width: 4.0,
            height: 4.0,
        });
        list.ops.push(PaintOp::PushOpacity {
            alpha: 0.5,
            x: 0.0,
            y: 0.0,
            width: 4.0,
            height: 4.0,
        });
        list.ops
            .push(op_fill(0.0, 0.0, 4.0, 4.0, [0.0; 8], [1.0, 0.0, 0.0, 1.0]));
        list.ops.push(PaintOp::PopOpacity);
        list.ops.push(PaintOp::PopBlend);
        let c = render(&list, 4, 4, [255, 255, 255, 255]);
        let mut p = [0u8; 4];
        p.copy_from_slice(&c.pixels[0..4]);
        assert_eq!(p, [191, 127, 127, 255], "blend 应包住 opacity 组");
        // 非可分离 luminosity：灰底 + 红源 → 亮度=0.3 → (77,77,77)
        assert_eq!(
            blend_pixel_of(
                BlendMode::Luminosity,
                [100, 100, 100, 255],
                [1.0, 0.0, 0.0, 1.0]
            ),
            [77, 77, 77, 255]
        );
    }
}
