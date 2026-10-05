//! 最小字体度量探测（A9）：从 sfnt/ttf 字节提取 ch/ex/ic 基准（每 em 归一）。
//!
//! 只读静态表：head（unitsPerEm/indexToLocFormat）、cmap（format 4）、
//! hhea/hmtx（advance）、loca+glyf 头（bbox 前 10 字节，无需轮廓解析）。
//! 探测失败一律 `None` → 引擎沿用近似缺省（ch/ex=0.5em、ic=1em，B 级在案）。
//! 与 style-engine-soft 的 soft::ttf 光栅化互不影响（本 crate 不依赖 soft）。

/// 探测产物（每 em 归一度量 + 族名表）。
pub(crate) struct ProbedFont {
    /// 族名（name 表 nameID 16/1，Windows UTF-16BE 优先）。
    pub names: Vec<String>,
    /// 度量。
    pub metrics: RawMetrics,
}

/// 探测产物（每 em 归一度量）。
pub(crate) struct RawMetrics {
    /// 数字 0 字形 advance（ch 基准）。
    pub ch_per_em: f32,
    /// x 字形 yMax（ex 基准）。
    pub ex_per_em: f32,
    /// 表意字 U+6C34 advance（ic 基准；缺字=1.0）。
    pub ic_per_em: f32,
}

fn be16(d: &[u8], off: usize) -> Option<u16> {
    let b = d.get(off..off + 2)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

fn be32(d: &[u8], off: usize) -> Option<u32> {
    let b = d.get(off..off + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// 探测字体度量。`data` 为完整 sfnt/ttf 字节（ttc 集合不支持→None）。
pub(crate) fn probe_metrics(data: &[u8]) -> Option<ProbedFont> {
    // sfnt 偏移表：u32 sfntVersion、u16 numTables…；表记录 16 字节
    if data.len() < 12 || be32(data, 0)? != 0x0001_0000 {
        return None;
    }
    let num_tables = be16(data, 4)? as usize;
    if 12 + num_tables * 16 > data.len() {
        return None;
    }
    let (mut head, mut cmap, mut hhea, mut hmtx, mut loca, mut glyf, mut name_off) =
        (None, None, None, None, None, None, None);
    for i in 0..num_tables {
        let rec = 12 + i * 16;
        let tag = &data[rec..rec + 4];
        let off = be32(data, rec + 8)? as usize;
        match tag {
            b"head" => head = Some(off),
            b"cmap" => cmap = Some(off),
            b"hhea" => hhea = Some(off),
            b"hmtx" => hmtx = Some(off),
            b"loca" => loca = Some(off),
            b"glyf" => glyf = Some(off),
            b"name" => name_off = Some(off),
            _ => {}
        }
    }
    let head = head?;
    // head: unitsPerEm @18（u16）、indexToLocFormat @50（0=short 1=long）
    let upem = be16(data, head + 18)? as f32;
    if upem <= 0.0 {
        return None;
    }
    let index_to_loc = be16(data, head + 50)?;
    // cmap：优先 (3,1) Windows/BMP，其次任意 format 4 子表
    let cmap = cmap?;
    let n_sub = be16(data, cmap + 2)? as usize;
    let mut sub = None;
    for i in 0..n_sub {
        let rec = cmap + 4 + i * 8;
        let plat = be16(data, rec)?;
        let enc = be16(data, rec + 2)?;
        let off = cmap + be32(data, rec + 4)? as usize;
        if be16(data, off)? == 4 {
            match (plat, enc) {
                (3, 1) => {
                    sub = Some(off);
                    break;
                }
                _ => sub = sub.or(Some(off)),
            }
        }
    }
    let sub = sub?;
    // format 4：segCountX2 @6；endCode[]@14、startCode[]、idDelta[]、
    // idRangeOffset[] 依次排布（中间隔 reservedPad=2 字节）
    let seg_x2 = be16(data, sub + 6)? as usize;
    let segs = seg_x2 / 2;
    let ends = sub + 14;
    let starts = ends + seg_x2 + 2;
    let deltas = starts + seg_x2;
    let ranges = deltas + seg_x2;
    let gid_of = |cp: u16| -> Option<u16> {
        for s in 0..segs {
            let end = be16(data, ends + s * 2)?;
            let start = be16(data, starts + s * 2)?;
            if (start..=end).contains(&cp) {
                let ro = be16(data, ranges + s * 2)?;
                let gid = if ro == 0 {
                    let delta = be16(data, deltas + s * 2)? as i16 as i32;
                    let g = (cp as i32 + delta) & 0xFFFF;
                    g as u16
                } else {
                    let gaddr = ranges + s * 2 + ro as usize + (cp - start) as usize * 2;
                    let g = be16(data, gaddr)?;
                    if g == 0 {
                        0
                    } else {
                        let delta = be16(data, deltas + s * 2)? as i16 as i32;
                        ((g as i32 + delta) & 0xFFFF) as u16
                    }
                };
                return Some(gid);
            }
        }
        None
    };
    // hhea: numberOfHMetrics @34；hmtx: 第 i 项 advanceWidth @i*4（越界用末项）
    let n_hm = be16(data, hhea? + 34)? as usize;
    let hmtx = hmtx?;
    let advance_of = |gid: u16| -> Option<f32> {
        let g = gid as usize;
        let idx = if n_hm == 0 {
            return None;
        } else if g < n_hm {
            g
        } else {
            n_hm - 1
        };
        Some(be16(data, hmtx + idx * 4)? as f32 / upem)
    };
    // glyf 头 bbox：numberOfContours@0、xMin@2、yMin@4、xMax@6、yMax@8；
    // 空字形（loca 相邻相等）无 bbox → None
    let y_max_of = |gid: u16| -> Option<f32> {
        let loca = loca?;
        let glyf = glyf?;
        let (start, end) = if index_to_loc == 0 {
            (
                be16(data, loca + gid as usize * 2)? as usize * 2,
                be16(data, loca + (gid as usize + 1) * 2)? as usize * 2,
            )
        } else {
            (
                be32(data, loca + gid as usize * 4)? as usize,
                be32(data, loca + (gid as usize + 1) * 4)? as usize,
            )
        };
        if start == end {
            return None;
        }
        let g = glyf + start;
        Some(be16(data, g + 8)? as i16 as f32 / upem)
    };
    let ch_per_em = advance_of(gid_of(0x30)?).unwrap_or(0.5);
    let ex_per_em = y_max_of(gid_of(0x78)?).unwrap_or(0.5).max(0.0);
    let ic_per_em = gid_of(0x6C34).and_then(advance_of).unwrap_or(1.0);
    // name 表：count @2、stringOffset @4；记录 12B（platform@0, enc@2,
    // lang@4, nameID@6, len@8, off@10）——取 nameID 16（首选）与 1（次之）
    let mut names = Vec::new();
    if let Some(name) = name_off
        && let Some(cnt) = be16(data, name + 2).map(|v| v as usize)
    {
        let storage = name + be16(data, name + 4)? as usize;
        let mut push = |s: Option<String>| {
            if let Some(s) = s
                && !s.is_empty()
                && !names.contains(&s)
            {
                names.push(s);
            }
        };
        for i in 0..cnt {
            let rec = name + 6 + i * 12;
            let (Some(plat), Some(nid)) = (be16(data, rec), be16(data, rec + 6)) else {
                continue;
            };
            if nid != 1 && nid != 16 {
                continue;
            }
            let Some(len) = be16(data, rec + 8).map(|v| v as usize) else {
                continue;
            };
            let Some(so) = be16(data, rec + 10).map(|v| v as usize) else {
                continue;
            };
            let Some(bytes) = data.get(storage + so..storage + so + len) else {
                continue;
            };
            let s = match plat {
                0 | 3 => {
                    let units: Vec<u16> = bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_be_bytes([c[0], c[1]]))
                        .collect();
                    String::from_utf16_lossy(&units)
                }
                _ => bytes.iter().map(|&b| b as char).collect(),
            };
            push(Some(s.trim().to_string()));
        }
    }
    Some(ProbedFont {
        names,
        metrics: RawMetrics {
            ch_per_em,
            ex_per_em,
            ic_per_em,
        },
    })
}
