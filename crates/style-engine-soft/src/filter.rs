//! Per-pixel pipeline for the CSS filter function family (P2 batch,
//! ADR-0031 D4).
//!
//! Working domain: function filters operate in the **sRGB non-premultiplied**
//! domain (css-filters-1 §3; the canvas itself stores non-premultiplied RGBA,
//! color matrices act on RGB directly, and their alpha row stays identity).
//! Blur executes in the **premultiplied domain** (colors from transparent
//! regions do not bleed in), un-premultiplying on the way in and out. Reuses
//! the P1-4 box blur infrastructure (three-pass separable, u32 window
//! rounding, bit-exact deterministic). Wiring into
//! `PaintOp::PushFilter`/`BackdropFilter` landed with the P2 batch.

use style_engine::paint::FilterEffect;

use crate::{blur_alpha_u8, box_width_for_sigma};

/// Applies an effect chain, one effect at a time, to a whole RGBA
/// (non-premultiplied sRGB) buffer — in declaration order (the chain order is
/// the semantic order). The blur/drop-shadow radius→σ conversion happens here
/// (css-filters-1 §3: σ = radius/2); matrix-family effects run in the
/// non-premultiplied domain, while blur/shadow premultiply internally.
pub fn apply_effects(buf: &mut [u8], w: usize, h: usize, effects: &[FilterEffect]) {
    for f in effects {
        match f {
            FilterEffect::Blur(r) => blur_rgba(buf, w, h, r / 2.0),
            FilterEffect::DropShadow {
                dx,
                dy,
                blur,
                color,
            } => drop_shadow(
                buf,
                w,
                h,
                dx.round() as i64,
                dy.round() as i64,
                blur / 2.0,
                color.components,
            ),
            FilterEffect::Opacity(v) => apply_matrix(buf, &matrix_opacity(*v)),
            FilterEffect::HueRotate(d) => apply_matrix(buf, &matrix_hue_rotate(*d)),
            FilterEffect::Brightness(v) => apply_matrix(buf, &matrix_brightness(*v)),
            FilterEffect::Contrast(v) => apply_matrix(buf, &matrix_contrast(*v)),
            FilterEffect::Grayscale(v) => apply_matrix(buf, &matrix_grayscale(*v)),
            FilterEffect::Sepia(v) => apply_matrix(buf, &matrix_sepia(*v)),
            FilterEffect::Saturate(v) => apply_matrix(buf, &matrix_saturate(*v)),
            FilterEffect::Invert(v) => apply_matrix(buf, &matrix_invert(*v)),
            // non_exhaustive 前向兼容：未知效果跳过（恒等）
            _ => {}
        }
    }
}

/// `opacity(a)`: alpha scaling matrix (RGB identity).
pub fn matrix_opacity(a: f32) -> ColorMatrix {
    let mut m = matrix_identity();
    m[18] = a.clamp(0.0, 1.0);
    m
}

/// Outset padding in px required on all four sides for an effect chain:
/// blur 3σ = 1.5·radius; drop-shadow offset + 1.5·blur radius. The pad is
/// used for the offscreen layer and backdrop sampling (samples outside the
/// region participate in the blur).
pub fn effects_pad(effects: &[FilterEffect]) -> f32 {
    let mut pad = 0.0f32;
    for f in effects {
        match f {
            FilterEffect::Blur(r) => pad = pad.max(1.5 * r),
            FilterEffect::DropShadow { dx, dy, blur, .. } => {
                pad = pad.max(dx.abs() + 1.5 * blur);
                pad = pad.max(dy.abs() + 1.5 * blur);
            }
            _ => {}
        }
    }
    pad
}

/// feColorMatrix, 5×4 row-major (20 values): each output channel is a linear
/// combination of the 4 input channels plus an offset. Applied per pixel in
/// the non-premultiplied sRGB domain; matrix-family alpha rows are identity
/// (in css-filters-1 §4 every function matrix has the alpha row `0 0 0 1 0`).
pub type ColorMatrix = [f32; 20];

/// Applies a color matrix per pixel (non-premultiplied domain; all RGB+A
/// channels; matrix-family alpha row is identity).
pub fn apply_matrix(buf: &mut [u8], m: &ColorMatrix) {
    for px in buf.as_chunks_mut::<4>().0 {
        let i = [
            px[0] as f32 / 255.0,
            px[1] as f32 / 255.0,
            px[2] as f32 / 255.0,
            px[3] as f32 / 255.0,
        ];
        for (c, row) in px.iter_mut().enumerate().take(4) {
            let k = row_start(c);
            let v = m[k] * i[0] + m[k + 1] * i[1] + m[k + 2] * i[2] + m[k + 3] * i[3] + m[k + 4];
            *row = (v * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

fn row_start(c: usize) -> usize {
    c * 5
}

/// Identity matrix (identity filter).
pub fn matrix_identity() -> ColorMatrix {
    let mut m = [0.0; 20];
    m[0] = 1.0;
    m[6] = 1.0;
    m[12] = 1.0;
    m[18] = 1.0;
    m
}

/// `grayscale(amount)`: linear interpolation from identity to the luminance
/// matrix (css-filters-1 §4; sRGB luminance coefficients 0.2126/0.7152/0.0722).
pub fn matrix_grayscale(s: f32) -> ColorMatrix {
    interpolate(&matrix_identity(), &raw_grayscale(), s)
}

/// `sepia(amount)`: linear interpolation from identity to the sepia matrix.
pub fn matrix_sepia(s: f32) -> ColorMatrix {
    interpolate(&matrix_identity(), &raw_sepia(), s)
}

/// `saturate(s)`: parameterized matrix from css-filters-1 §4.
pub fn matrix_saturate(s: f32) -> ColorMatrix {
    let mut m = [0.0; 20];
    let (r, g, b) = (0.213, 0.715, 0.072);
    m[0] = r + (1.0 - r) * s;
    m[1] = g - g * s;
    m[2] = b - b * s;
    m[5] = r - r * s;
    m[6] = g + (1.0 - g) * s;
    m[7] = b - b * s;
    m[10] = r - r * s;
    m[11] = g - g * s;
    m[12] = b + (1.0 - b) * s;
    m[18] = 1.0;
    m
}

/// `hue-rotate(deg)`: cosine/sine parameterized matrix from css-filters-1 §4
/// (a linear approximation in sRGB — the spec matrix is implemented as-is).
pub fn matrix_hue_rotate(deg: f32) -> ColorMatrix {
    let rad = deg.to_radians();
    let (cos, sin) = (rad.cos(), rad.sin());
    let mut m = [0.0; 20];
    // r' 行
    m[0] = 0.213 + cos * 0.787 - sin * 0.213;
    m[1] = 0.715 - cos * 0.715 - sin * 0.715;
    m[2] = 0.072 - cos * 0.072 + sin * 0.928;
    // g' 行
    m[5] = 0.213 - cos * 0.213 + sin * 0.143;
    m[6] = 0.715 + cos * 0.285 + sin * 0.140;
    m[7] = 0.072 - cos * 0.072 - sin * 0.283;
    // b' 行
    m[10] = 0.213 - cos * 0.213 - sin * 0.787;
    m[11] = 0.715 - cos * 0.715 + sin * 0.715;
    m[12] = 0.072 + cos * 0.928 + sin * 0.072;
    m[18] = 1.0;
    m
}

/// `invert(amount)`: `c' = (1−2a)·c + a` (a=0 identity, a=1 full inversion);
/// alpha is untouched.
pub fn matrix_invert(a: f32) -> ColorMatrix {
    let mut m = [0.0; 20];
    let d = 1.0 - 2.0 * a;
    m[0] = d;
    m[4] = a;
    m[6] = d;
    m[9] = a;
    m[12] = d;
    m[14] = a;
    m[18] = 1.0;
    m
}

/// `brightness(amount)`: `c' = amount·c` (diagonal scaling of RGB).
pub fn matrix_brightness(a: f32) -> ColorMatrix {
    let mut m = [0.0; 20];
    m[0] = a;
    m[6] = a;
    m[12] = a;
    m[18] = 1.0;
    m
}

/// `contrast(amount)`: `c' = amount·(c − 0.5) + 0.5`.
pub fn matrix_contrast(a: f32) -> ColorMatrix {
    let mut m = [0.0; 20];
    m[0] = a;
    m[4] = 0.5 - 0.5 * a;
    m[6] = a;
    m[9] = 0.5 - 0.5 * a;
    m[12] = a;
    m[14] = 0.5 - 0.5 * a;
    m[18] = 1.0;
    m
}

fn raw_grayscale() -> ColorMatrix {
    let mut m = [0.0; 20];
    let (r, g, b) = (0.2126, 0.7152, 0.0722);
    for row in 0..3 {
        m[row * 5] = r;
        m[row * 5 + 1] = g;
        m[row * 5 + 2] = b;
    }
    m[18] = 1.0;
    m
}

fn raw_sepia() -> ColorMatrix {
    let mut m = [0.0; 20];
    m[0] = 0.393;
    m[1] = 0.769;
    m[2] = 0.189;
    m[5] = 0.349;
    m[6] = 0.686;
    m[7] = 0.168;
    m[10] = 0.272;
    m[11] = 0.534;
    m[12] = 0.131;
    m[18] = 1.0;
    m
}

/// Linear interpolation from identity to a target matrix (css-filters-1
/// grayscale/sepia semantics).
fn interpolate(i: &ColorMatrix, t: &ColorMatrix, s: f32) -> ColorMatrix {
    let mut m = [0.0; 20];
    for k in 0..20 {
        m[k] = i[k] + (t[k] - i[k]) * s;
    }
    m
}

/// Box blur of a whole RGBA (non-premultiplied) buffer: planes are flattened
/// to premultiplied form → three-pass separable box blur per plane →
/// un-premultiplied on write-back. Colors from transparent regions do not
/// bleed in (the blur runs in the premultiplied domain). The caller is
/// responsible for padding (pass in the region outset by ⌈3σ⌉, as in P1-4).
pub fn blur_rgba(buf: &mut [u8], w: usize, h: usize, sigma: f32) {
    if sigma <= 0.0 || w == 0 || h == 0 || box_width_for_sigma(sigma) <= 1 {
        return;
    }
    let n = w * h;
    let mut planes: [Vec<u8>; 4] = std::array::from_fn(|_| vec![0u8; n]);
    for (i, px) in buf.as_chunks::<4>().0.iter().enumerate() {
        let a = px[3] as u32;
        for c in 0..3 {
            // 预乘：c·a（/255 归一回 u8 域）
            planes[c][i] = ((px[c] as u32 * a + 127) / 255) as u8;
        }
        planes[3][i] = px[3];
    }
    for p in &mut planes {
        blur_alpha_u8(p, w, h, sigma);
    }
    for (i, px) in buf.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let a = planes[3][i] as u32;
        // 整数精确除法（u32 累加器），保逐位确定——不用 checked 形态
        #[allow(clippy::manual_checked_ops)]
        if a == 0 {
            // 全透明：un-premultiply 无定义，归零（含 RGB，避免残色）
            *px = [0; 4];
        } else {
            for c in 0..3 {
                // un-premultiply：c = (c·a)·255 / a（钳 255 防 u8 预乘舍入上溢）
                px[c] = ((planes[c][i] as u32 * 255 + a / 2) / a).min(255) as u8;
            }
            px[3] = planes[3][i];
        }
    }
}

/// `drop-shadow` (css-filters-1): the shadow = source alpha mask → box blur
/// (σ=blur/2, converted by the caller) → translate (dx, dy) → tint → composite
/// **shadow first, then source** (out = source over shadow; the shadow is
/// painted beneath the source). `color` is non-premultiplied sRGB in 0..1.
/// The caller is responsible for the bbox outset (dx/dy/3σ).
pub fn drop_shadow(
    buf: &mut [u8],
    w: usize,
    h: usize,
    dx: i64,
    dy: i64,
    sigma: f32,
    color: [f32; 4],
) {
    let n = w * h;
    if n == 0 {
        return;
    }
    // ① 源 alpha 遮罩 → 模糊（P1-4 同核）
    let mut mask = vec![0u8; n];
    for (i, px) in buf.as_chunks::<4>().0.iter().enumerate() {
        mask[i] = px[3];
    }
    if sigma > 0.0 && box_width_for_sigma(sigma) > 1 {
        blur_alpha_u8(&mut mask, w, h, sigma);
    }
    // ② 影层：透明底 + 平移着色（非预乘：rgb = color.rgb，a = color.a·mask）
    let mut shadow = vec![0u8; n * 4];
    let cr = (color[0] * 255.0).round().clamp(0.0, 255.0);
    let cg = (color[1] * 255.0).round().clamp(0.0, 255.0);
    let cb = (color[2] * 255.0).round().clamp(0.0, 255.0);
    let ca = color[3].clamp(0.0, 1.0);
    for y in 0..h {
        for x in 0..w {
            let sx = x as i64 - dx;
            let sy = y as i64 - dy;
            if sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 {
                continue;
            }
            let m = mask[sy as usize * w + sx as usize] as f32 / 255.0;
            let a = (ca * m * 255.0).round().clamp(0.0, 255.0) as u8;
            let o = (y * w + x) * 4;
            shadow[o] = cr as u8;
            shadow[o + 1] = cg as u8;
            shadow[o + 2] = cb as u8;
            shadow[o + 3] = a;
        }
    }
    // ③ out = source over shadow（非预乘 src-over：co = (cs·as + cb·ab·(1−as))/ao）
    let mut out = vec![0u8; n * 4];
    for i in 0..n {
        let s = &buf[i * 4..i * 4 + 4];
        let b = &shadow[i * 4..i * 4 + 4];
        let (as_f, ab) = (s[3] as f32 / 255.0, b[3] as f32 / 255.0);
        let ao = as_f + ab * (1.0 - as_f);
        out[i * 4 + 3] = (ao * 255.0).round().clamp(0.0, 255.0) as u8;
        if ao <= 0.0 {
            continue;
        }
        for c in 0..3 {
            let cs = s[c] as f32 / 255.0;
            let cbb = b[c] as f32 / 255.0;
            let co = (cs * as_f + cbb * ab * (1.0 - as_f)) / ao;
            out[i * 4 + c] = (co * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    buf.copy_from_slice(&out);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(buf: &[u8], x: usize, y: usize, w: usize) -> [u8; 4] {
        let o = (y * w + x) * 4;
        [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]
    }

    fn solid(r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
        vec![r, g, b, a]
    }

    #[test]
    fn invert_full_and_alpha_preserved() {
        let mut buf = solid(100, 150, 200, 255);
        apply_matrix(&mut buf, &matrix_invert(1.0));
        assert_eq!(buf, vec![155, 105, 55, 255]);
        // 半透明：alpha 不动、颜色仍反色（非预乘域）
        let mut buf = solid(100, 150, 200, 128);
        apply_matrix(&mut buf, &matrix_invert(1.0));
        assert_eq!(buf, vec![155, 105, 55, 128]);
    }

    #[test]
    fn grayscale_full_luma() {
        let mut buf = solid(255, 0, 0, 255);
        apply_matrix(&mut buf, &matrix_grayscale(1.0));
        // 0.2126·255 = 54.2 → round 54
        assert_eq!(buf[0], buf[1]);
        assert_eq!(buf[1], buf[2]);
        assert_eq!(buf[0], 54);
    }

    #[test]
    fn brightness_zero_black_alpha_kept() {
        let mut buf = solid(200, 100, 50, 255);
        apply_matrix(&mut buf, &matrix_brightness(0.0));
        assert_eq!(buf, vec![0, 0, 0, 255]);
    }

    #[test]
    fn chain_order_matters() {
        // 链序非交换性：invert(1)→brightness(0.5) ≠ brightness(0.5)→invert(1)
        let mut a = solid(200, 100, 50, 255);
        apply_matrix(&mut a, &matrix_invert(1.0));
        apply_matrix(&mut a, &matrix_brightness(0.5));
        let mut b = solid(200, 100, 50, 255);
        apply_matrix(&mut b, &matrix_brightness(0.5));
        apply_matrix(&mut b, &matrix_invert(1.0));
        assert_ne!(a[..3], b[..3], "函数链序必须保持（css-filters-1 有序表）");
    }

    #[test]
    fn blur_rgba_premultiply_no_color_bleed() {
        // 左半不透明红、右半全透明：模糊后透明侧 RGB ≈ 红（预乘域模糊
        // 防黑渗）且 alpha 单调衰减
        let (w, h) = (32usize, 8usize);
        let mut buf = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let o = (y * w + x) * 4;
                if x < w / 2 {
                    buf[o] = 255;
                    buf[o + 3] = 255;
                }
            }
        }
        blur_rgba(&mut buf, w, h, 2.0);
        let mid = px(&buf, w / 2 + 2, h / 2, w);
        assert!(mid[3] > 0 && mid[3] < 255, "边缘 alpha 衰减：{mid:?}");
        assert!(mid[0] > 200, "透明侧颜色≈红（无黑渗）：{mid:?}");
        let deep = px(&buf, w - 1, h / 2, w);
        assert_eq!(
            deep[3], 0,
            "远端应无墨（pad 由调用方保证，此测试 3σ=6 ≥ 12px? 无）"
        );
    }

    #[test]
    fn drop_shadow_offset_colored_under_source() {
        // 源方块 (8..24, 8..24) 不透明白，影偏移 (24, 0) 蓝色
        let (w, h) = (64usize, 32usize);
        let mut buf = vec![0u8; w * h * 4];
        for y in 8..24 {
            for x in 8..24 {
                let o = (y * w + x) * 4;
                buf[o] = 255;
                buf[o + 1] = 255;
                buf[o + 2] = 255;
                buf[o + 3] = 255;
            }
        }
        drop_shadow(&mut buf, w, h, 24, 0, 0.0, [0.0, 0.0, 1.0, 1.0]);
        // 影区（源右侧）：蓝
        let sh = px(&buf, 40, 16, w);
        assert_eq!(&sh[..3], &[0, 0, 255], "影着色：{sh:?}");
        // 源区：源 over 影 → 源色（白）不变
        let src = px(&buf, 16, 16, w);
        assert_eq!(&src[..3], &[255, 255, 255], "源叠影上：{src:?}");
        // 空白区：无墨
        let empty = px(&buf, 2, 2, w);
        assert_eq!(empty[3], 0);
    }

    #[test]
    fn drop_shadow_sigma_softens() {
        // σ>0：影边缘软化（锐利核心外 alpha 显著低于核心）
        let (w, h) = (64usize, 32usize);
        let mut buf = vec![0u8; w * h * 4];
        for y in 8..24 {
            for x in 8..24 {
                let o = (y * w + x) * 4;
                buf[o + 3] = 255;
            }
        }
        drop_shadow(&mut buf, w, h, 0, 0, 2.0, [1.0, 0.0, 0.0, 1.0]);
        let core = px(&buf, 16, 16, w)[3];
        let fringe = px(&buf, 28, 16, w)[3];
        assert!(core > 200, "影核心近源 alpha：{core}");
        assert!(
            fringe > 0 && fringe < core,
            "边缘软化：core={core} fringe={fringe}"
        );
    }
}
