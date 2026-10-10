//! Minimal TrueType reader (pure std): the font source for soft-sink text
//! rasterization.
//!
//! Coverage: sfnt table directory, head/hhea/maxp headers, hmtx advances,
//! cmap format 4 (BMP Unicode), loca/glyf simple and composite glyphs
//! (translation + uniform/two-axis scaling; point-matched components are not
//! supported). Quadratic curves are flattened to a fixed 16 segments per
//! curve (deviation ≲0.06px at 16px with 2048 upm). No kerning/GSUB/hinting
//! at all (documented deviation — the advance difference vs. parley
//! measurements counts as Class 1/2 budget noise).
//!
//! Metrics conventions align with Chromium/engine ㉔: ascender/descender come
//! from hhea (DejaVu matches the usWin values); normal line height =
//! round(asc)+round(desc).

/// Flattened glyph outlines (font units, y-up; each outline is a closed
/// polygon vertex sequence).
pub type Outline = Vec<Vec<(f32, f32)>>;

/// Segments used to flatten a quadratic curve (each curve → 16 line segments).
const QUAD_SUBDIV: usize = 16;

/// Accumulated affine for glyph expansion (two-axis scaling + translation, in
/// font units; composed per level for composite glyphs).
#[derive(Clone, Copy)]
struct Xform {
    sxx: f32,
    syy: f32,
    dx: f32,
    dy: f32,
}

impl Xform {
    const IDENTITY: Self = Self {
        sxx: 1.0,
        syy: 1.0,
        dx: 0.0,
        dy: 0.0,
    };

    #[inline]
    fn map(&self, x: f32, y: f32) -> (f32, f32) {
        (self.dx + self.sxx * x, self.dy + self.syy * y)
    }

    /// Component transform composition: child component coordinates pass
    /// through (csx,csy,cdx,cdy) first, then through self.
    #[inline]
    fn compose(&self, csx: f32, csy: f32, cdx: f32, cdy: f32) -> Self {
        Self {
            sxx: self.sxx * csx,
            syy: self.syy * csy,
            dx: self.dx + self.sxx * cdx,
            dy: self.dy + self.syy * cdy,
        }
    }
}

pub struct SoftFont<'a> {
    data: &'a [u8],
    units_per_em: u16,
    index_to_loc: u16,
    num_glyphs: u16,
    num_h_metrics: u16,
    pub ascender: i16,
    pub descender: i16,
    hmtx: usize,
    loca: usize,
    glyf: usize,
    /// Absolute offset of the selected Unicode cmap subtable (format 4).
    cmap_sub: usize,
}

#[inline]
fn r_u8(d: &[u8], off: usize) -> u8 {
    d.get(off).copied().unwrap_or(0)
}

#[inline]
fn r_u16(d: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([r_u8(d, off), r_u8(d, off + 1)])
}

#[inline]
fn r_i16(d: &[u8], off: usize) -> i16 {
    i16::from_be_bytes([r_u8(d, off), r_u8(d, off + 1)])
}

#[inline]
fn r_u32(d: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([
        r_u8(d, off),
        r_u8(d, off + 1),
        r_u8(d, off + 2),
        r_u8(d, off + 3),
    ])
}

impl<'a> SoftFont<'a> {
    /// Parses the sfnt header and the required tables (TrueType glyf
    /// outlines; CFF/OTTO is not supported).
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let ver = r_u32(data, 0);
        let is_ttf = ver == 0x0001_0000 || data.get(0..4) == Some(&b"true"[..]);
        if !is_ttf {
            return None;
        }
        let n_tables = r_u16(data, 4) as usize;
        let (mut head, mut hhea, mut maxp, mut hmtx, mut loca, mut glyf, mut cmap) =
            (None, None, None, None, None, None, None);
        for i in 0..n_tables {
            let base = 12 + i * 16;
            let tag = data.get(base..base + 4)?;
            let off = r_u32(data, base + 8) as usize;
            match tag {
                b"head" => head = Some(off),
                b"hhea" => hhea = Some(off),
                b"maxp" => maxp = Some(off),
                b"hmtx" => hmtx = Some(off),
                b"loca" => loca = Some(off),
                b"glyf" => glyf = Some(off),
                b"cmap" => cmap = Some(off),
                _ => {}
            }
        }
        let head = head?;
        let hhea = hhea?;
        let maxp = maxp?;
        let hmtx = hmtx?;
        let loca = loca?;
        let glyf = glyf?;
        let cmap = cmap?;
        let units_per_em = r_u16(data, head + 18);
        if units_per_em == 0 {
            return None;
        }
        let index_to_loc = r_u16(data, head + 50);
        let ascender = r_i16(data, hhea + 4);
        let descender = r_i16(data, hhea + 6);
        let num_h_metrics = r_u16(data, hhea + 34);
        let num_glyphs = r_u16(data, maxp + 4);
        let cmap_sub = Self::pick_cmap_subtable(data, cmap)?;
        Some(Self {
            data,
            units_per_em,
            index_to_loc,
            num_glyphs,
            num_h_metrics,
            ascender,
            descender,
            hmtx,
            loca,
            glyf,
            cmap_sub,
        })
    }

    /// Picks a Unicode subtable: prefers (3,1) Windows BMP, then (0,*), then
    /// the first available.
    fn pick_cmap_subtable(data: &[u8], cmap: usize) -> Option<usize> {
        let n = r_u16(data, cmap + 2) as usize;
        let mut fallback = None;
        let mut unicode_any = None;
        for i in 0..n {
            let rec = cmap + 4 + i * 8;
            let platform = r_u16(data, rec);
            let encoding = r_u16(data, rec + 2);
            let off = cmap + r_u32(data, rec + 4) as usize;
            if (platform, encoding) == (3, 1) {
                return Some(off);
            }
            if platform == 0 && unicode_any.is_none() {
                unicode_any = Some(off);
            }
            if fallback.is_none() {
                fallback = Some(off);
            }
        }
        unicode_any.or(fallback)
    }

    /// Font size → font-unit scaling factor.
    pub fn scale_for(&self, font_size: f32) -> f32 {
        font_size / f32::from(self.units_per_em)
    }

    /// Glyph advance width (px).
    pub fn advance(&self, gid: u16, scale: f32) -> f32 {
        let idx = gid.min(self.num_h_metrics.saturating_sub(1)) as usize;
        f32::from(r_u16(self.data, self.hmtx + idx * 4)) * scale
    }

    /// cmap format 4 code-point lookup (unmapped returns notdef=0 for the
    /// caller to decide).
    pub fn lookup(&self, ch: char) -> Option<u16> {
        let c = u32::from(ch);
        if c > 0xFFFF {
            return None;
        }
        let c = c as u16;
        let s = self.cmap_sub;
        if r_u16(self.data, s) != 4 {
            return None;
        }
        let seg_count = r_u16(self.data, s + 6) as usize / 2;
        let end_base = s + 14;
        let start_base = end_base + seg_count * 2 + 2;
        let delta_base = start_base + seg_count * 2;
        let range_base = delta_base + seg_count * 2;
        for i in 0..seg_count {
            let end = r_u16(self.data, end_base + i * 2);
            if c > end {
                continue;
            }
            let start = r_u16(self.data, start_base + i * 2);
            if c < start {
                return None;
            }
            let delta = r_i16(self.data, delta_base + i * 2);
            let range = r_u16(self.data, range_base + i * 2);
            let g = if range == 0 {
                // idDelta 位型回绕（mod 65536）
                c.wrapping_add(delta as u16)
            } else {
                let addr = range_base + i * 2 + range as usize + (c - start) as usize * 2;
                let g = r_u16(self.data, addr);
                if g == 0 {
                    return None;
                }
                g.wrapping_add(delta as u16)
            };
            return if g == 0 { None } else { Some(g) };
        }
        None
    }

    /// Glyph outlines (font-unit polygons; composite glyphs are expanded
    /// recursively).
    pub fn outline(&self, gid: u16) -> Outline {
        let mut out = Vec::new();
        self.outline_rec(gid, Xform::IDENTITY, &mut out, 0);
        out
    }

    fn loca_range(&self, gid: u16) -> (usize, usize) {
        let i = gid as usize;
        if self.index_to_loc == 0 {
            (
                r_u16(self.data, self.loca + i * 2) as usize * 2,
                r_u16(self.data, self.loca + i * 2 + 2) as usize * 2,
            )
        } else {
            (
                r_u32(self.data, self.loca + i * 4) as usize,
                r_u32(self.data, self.loca + i * 4 + 4) as usize,
            )
        }
    }

    fn outline_rec(&self, gid: u16, xf: Xform, out: &mut Outline, depth: u8) {
        if depth > 8 || gid >= self.num_glyphs {
            return;
        }
        let (o0, o1) = self.loca_range(gid);
        if o1 <= o0 {
            return;
        }
        let g = self.glyf + o0;
        let nc = r_i16(self.data, g);
        if nc >= 0 {
            self.simple_glyph(g, nc as usize, xf, out);
        } else {
            self.composite_glyph(g, xf, out, depth);
        }
    }

    /// Simple glyph: flags/x/y decoding → closed polygon per outline (implicit
    /// on-curve midpoints + quadratic flattening).
    fn simple_glyph(&self, g: usize, n: usize, xf: Xform, out: &mut Outline) {
        let end_base = g + 10;
        let mut end_pts = Vec::with_capacity(n);
        for i in 0..n {
            end_pts.push(r_u16(self.data, end_base + i * 2));
        }
        let Some(&n_pts16) = end_pts.last() else {
            return;
        };
        let n_pts = n_pts16 as usize + 1;
        let instr_len = r_u16(self.data, end_base + n * 2) as usize;
        let mut p = end_base + n * 2 + 2 + instr_len;
        // flags（0x08 repeat 展开）
        let mut flags = Vec::with_capacity(n_pts);
        while flags.len() < n_pts {
            let f = r_u8(self.data, p);
            p += 1;
            flags.push(f);
            if f & 0x08 != 0 {
                let rep = r_u8(self.data, p);
                p += 1;
                for _ in 0..rep {
                    flags.push(f);
                }
            }
        }
        flags.truncate(n_pts);
        // x 坐标增量（0x02 短格式；0x10 符号/同前）
        let mut xs = vec![0i32; n_pts];
        let mut x = 0i32;
        for i in 0..n_pts {
            let f = flags[i];
            if f & 0x02 != 0 {
                let d = r_u8(self.data, p) as i32;
                p += 1;
                x += if f & 0x10 != 0 { d } else { -d };
            } else if f & 0x10 == 0 {
                x += i32::from(r_i16(self.data, p));
                p += 2;
            }
            xs[i] = x;
        }
        // y 坐标增量（0x04 短格式；0x20 符号/同前）
        let mut ys = vec![0i32; n_pts];
        let mut y = 0i32;
        for i in 0..n_pts {
            let f = flags[i];
            if f & 0x04 != 0 {
                let d = r_u8(self.data, p) as i32;
                p += 1;
                y += if f & 0x20 != 0 { d } else { -d };
            } else if f & 0x20 == 0 {
                y += i32::from(r_i16(self.data, p));
                p += 2;
            }
            ys[i] = y;
        }
        // 逐轮廓折线化
        let mut start = 0usize;
        for &end in &end_pts {
            let end = end as usize;
            if end < start || end >= n_pts {
                break;
            }
            let mut ring = Vec::with_capacity(end - start + 1);
            for i in start..=end {
                ring.push((xs[i], ys[i], flags[i] & 0x01 != 0));
            }
            let mut poly = ring_to_polygon(&ring);
            for pt in poly.iter_mut() {
                *pt = xf.map(pt.0, pt.1);
            }
            out.push(poly);
            start = end + 1;
        }
    }

    /// Composite glyph: recursive expansion with component translation (font
    /// units) + uniform/two-axis scaling; point-matched components and 2×2
    /// matrices are approximated with the identity matrix (documented
    /// deviation).
    fn composite_glyph(&self, g: usize, xf: Xform, out: &mut Outline, depth: u8) {
        let mut p = g + 10;
        loop {
            let flags = r_u16(self.data, p);
            let gid2 = r_u16(self.data, p + 2);
            p += 4;
            let (a1, a2) = if flags & 0x0001 != 0 {
                let v = (r_i16(self.data, p), r_i16(self.data, p + 2));
                p += 4;
                v
            } else {
                let v = (
                    i16::from(r_u8(self.data, p) as i8),
                    i16::from(r_u8(self.data, p + 1) as i8),
                );
                p += 2;
                v
            };
            if flags & 0x0002 == 0 {
                break; // 点匹配组件：不支持
            }
            let (csx, csy) = if flags & 0x0008 != 0 {
                let s = f32::from(r_i16(self.data, p)) / 16384.0;
                p += 2;
                (s, s)
            } else if flags & 0x0040 != 0 {
                let (x, y) = (
                    f32::from(r_i16(self.data, p)) / 16384.0,
                    f32::from(r_i16(self.data, p + 2)) / 16384.0,
                );
                p += 4;
                (x, y)
            } else {
                if flags & 0x0080 != 0 {
                    p += 8; // 2×2：跳过参数，按单位矩阵
                }
                (1.0, 1.0)
            };
            let (cdx, cdy) = (f32::from(a1), f32::from(a2));
            self.outline_rec(gid2, xf.compose(csx, csy, cdx, cdy), out, depth + 1);
            if flags & 0x0020 == 0 {
                break; // MORE_COMPONENTS 终止
            }
        }
    }
}

/// Closed point ring → polygon (implicit on-curve midpoints; quadratic curves
/// flattened to 16 segments). Input ring vertices are (x, y, on_curve), in
/// font units.
fn ring_to_polygon(ring: &[(i32, i32, bool)]) -> Vec<(f32, f32)> {
    let n = ring.len();
    if n == 0 {
        return Vec::new();
    }
    // 序列旋转到首个 on-curve 点；全 off 时合成首点 = 末点/首点中点。
    let pts: Vec<(f32, f32, bool)> = if let Some(k) = ring.iter().position(|p| p.2) {
        ring.iter()
            .cycle()
            .skip(k)
            .take(n)
            .map(|p| (p.0 as f32, p.1 as f32, p.2))
            .collect()
    } else {
        let mid = midpoint(
            (ring[n - 1].0 as f32, ring[n - 1].1 as f32),
            (ring[0].0 as f32, ring[0].1 as f32),
        );
        let mut v = vec![(mid.0, mid.1, true)];
        v.extend(ring.iter().map(|p| (p.0 as f32, p.1 as f32, p.2)));
        v
    };
    let m = pts.len();
    let mut poly = vec![(pts[0].0, pts[0].1)];
    let mut cur = (pts[0].0, pts[0].1);
    let mut i = 1usize;
    while i < m {
        let ctrl = pts[i];
        if ctrl.2 {
            poly.push((ctrl.0, ctrl.1));
            cur = (ctrl.0, ctrl.1);
            i += 1;
        } else {
            let q = pts[(i + 1) % m];
            let (end, next) = if q.2 {
                ((q.0, q.1), i + 2)
            } else {
                (midpoint((ctrl.0, ctrl.1), (q.0, q.1)), i + 1)
            };
            for k in 1..QUAD_SUBDIV {
                poly.push(quad_at(
                    cur,
                    (ctrl.0, ctrl.1),
                    end,
                    k as f32 / QUAD_SUBDIV as f32,
                ));
            }
            poly.push(end);
            cur = end;
            i = next;
        }
    }
    poly
}

#[inline]
fn midpoint(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5)
}

#[inline]
fn quad_at(p0: (f32, f32), c: (f32, f32), p1: (f32, f32), t: f32) -> (f32, f32) {
    let u = 1.0 - t;
    (
        u * u * p0.0 + 2.0 * u * t * c.0 + t * t * p1.0,
        u * u * p0.1 + 2.0 * u * t * c.1 + t * t * p1.1,
    )
}
