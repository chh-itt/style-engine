//! Numeric Channel（ADR-0003）：浏览器 `getBoundingClientRect` 数值基准
//! vs 引擎 `Frame` 盒，逐分量 0.5px 容差，零栅格化噪声。
//!
//! case 目录为单一事实源：`case.html`（结构 + data-key 标注）、
//! `case.css` 与 `manifest.toml`（视口/字体/特性子集/容差/xfail）。
//! 浏览器基准由 `tools/dump_rects.py`（Playwright）生成
//! `golden/numeric.json`，内含 Chrome 版本元数据；引擎侧由本模块直接
//! 消费同一对文件——输入中立性由「两侧只见过 case 文件」保证。

use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};
use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

/// fixture 节点：(data-key, 父下标, class 列表, 文本)。
pub type FixtureNode = (u64, Option<usize>, Vec<String>, Option<String>);

/// 每分量容差（默认 ADR-0003 的 0.5px）。
#[derive(Clone, Copy, Debug)]
pub struct Tolerance {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Default for Tolerance {
    fn default() -> Self {
        Self {
            x: 0.5,
            y: 0.5,
            w: 0.5,
            h: 0.5,
        }
    }
}

/// `manifest.toml`（v0 字段；缺省项均有合理默认）。
#[derive(Deserialize, Debug)]
pub struct CaseManifest {
    #[serde(default)]
    pub viewport: [f32; 2],
    #[serde(default)]
    pub scale: f32,
    /// 浏览器侧经 @font-face 强制加载的字体文件（相对仓库根）。
    #[serde(default)]
    pub fonts: Vec<String>,
    /// 所用特性子集（对照 docs/FEATURES.md 注册表；Class 3–5 零容忍的作用域）。
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub xfail: bool,
    #[serde(default)]
    pub tolerance: f64,
}

impl Default for CaseManifest {
    fn default() -> Self {
        Self {
            viewport: [800.0, 600.0],
            scale: 1.0,
            fonts: Vec::new(),
            features: Vec::new(),
            xfail: false,
            tolerance: 0.5,
        }
    }
}

/// 一个 numeric case：目录 + 解析产物。
pub struct NumericCase {
    pub dir: PathBuf,
    pub name: String,
    pub manifest: CaseManifest,
    pub css: String,
    /// (key, parent_index, classes, text)
    pub nodes: Vec<FixtureNode>,
}

impl NumericCase {
    pub fn load(dir: &Path) -> std::io::Result<Self> {
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let manifest: CaseManifest =
            toml::from_str(&std::fs::read_to_string(dir.join("manifest.toml"))?)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        let html = std::fs::read_to_string(dir.join("case.html"))?;
        let css = std::fs::read_to_string(dir.join("case.css"))?;
        let nodes = parse_fixture_divs(&html)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            name,
            manifest,
            css,
            nodes,
        })
    }

    pub fn golden_path(&self) -> PathBuf {
        self.dir.join("golden").join("numeric.json")
    }
}

/// 引擎侧产出盒（border box，与 getBoundingClientRect 同语义）。
#[derive(Clone, Copy, Debug)]
pub struct EngineBox {
    pub key: u64,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// golden/numeric.json 的一条记录。
#[derive(Deserialize, Debug)]
pub struct GoldenBox {
    pub key: u64,
    pub rect: [f64; 4],
}

#[derive(Deserialize, Debug)]
pub struct GoldenFile {
    pub meta: serde_json::Value,
    pub boxes: Vec<GoldenBox>,
}

/// 单分量超差记录。
#[derive(Debug)]
pub struct NumericDiff {
    pub key: u64,
    pub field: &'static str,
    pub engine: f64,
    pub golden: f64,
    pub delta: f64,
}

impl fmt::Display for NumericDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "key={} {} 引擎={:.3} 基准={:.3} Δ={:.3}",
            self.key, self.field, self.engine, self.golden, self.delta
        )
    }
}

/// 以敌意消费者标准（仅公共 API）从 case 文件构建引擎并取盒。
pub fn run_case(case: &NumericCase) -> Result<Vec<EngineBox>, String> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    if !engine.set_stylesheet(&case.css).is_clean() {
        return Err(format!("case {} 的 CSS 未干净解析", case.name));
    }
    for (i, (key, parent, classes, text)) in case.nodes.iter().enumerate() {
        let _ = i;
        engine
            .insert(
                parent.map(|p| case.nodes[p].0),
                *key,
                StyleNode {
                    name: Some("div".into()),
                    classes: classes.iter().cloned().collect(),
                    text: text.clone(),
                    ..Default::default()
                },
            )
            .map_err(|e| format!("case {} key {key} 插入失败: {e}", case.name))?;
    }
    let frame = engine.frame(
        (case.manifest.viewport[0], case.manifest.viewport[1]),
        case.manifest.scale,
        0.0,
    );
    Ok(case
        .nodes
        .iter()
        .map(|(key, ..)| {
            let b = frame.find(*key).expect("引擎盒缺失（数据 key 未落框）");
            EngineBox {
                key: *key,
                x: b.x,
                y: b.y,
                w: b.width,
                h: b.height,
            }
        })
        .collect())
}

/// 逐分量对比；返回全部超差记录（空 = 通过）。
pub fn diff(engine: &[EngineBox], golden: &[GoldenBox], tol: Tolerance) -> Vec<NumericDiff> {
    let mut diffs = Vec::new();
    for g in golden {
        let Some(e) = engine.iter().find(|b| b.key == g.key) else {
            diffs.push(NumericDiff {
                key: g.key,
                field: "missing",
                engine: f64::NAN,
                golden: g.rect[0],
                delta: f64::NAN,
            });
            continue;
        };
        let pairs = [
            ("x", f64::from(e.x), g.rect[0], tol.x),
            ("y", f64::from(e.y), g.rect[1], tol.y),
            ("w", f64::from(e.w), g.rect[2], tol.w),
            ("h", f64::from(e.h), g.rect[3], tol.h),
        ];
        for (field, ev, gv, t) in pairs {
            let d = (ev - gv).abs();
            if d > t {
                diffs.push(NumericDiff {
                    key: g.key,
                    field,
                    engine: ev,
                    golden: gv,
                    delta: d,
                });
            }
        }
    }
    diffs
}

/// 迷你 fixture 解析：仅支持 `<div class="a b" data-key="N">` 嵌套 +
/// 文本节点（white-space:normal 折叠语义）；其余标签/内容忽略。
/// 这是约定输入格式而非通用 HTML 解析——用例必须遵守（见 docs）。
pub fn parse_fixture_divs(html: &str) -> Result<Vec<FixtureNode>, String> {
    let clean = strip_comments(html);
    let bytes = clean.as_bytes();
    let mut i = 0usize;
    let mut nodes: Vec<FixtureNode> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut text_buf = String::new();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            flush_text(&mut text_buf, &mut nodes, &stack);
            let Some(close) = clean[i..].find('>') else {
                return Err("未闭合标签".into());
            };
            let tag = &clean[i + 1..i + close];
            i += close + 1;
            if let Some(name) = tag.strip_prefix('/') {
                if name.trim().eq_ignore_ascii_case("div") {
                    stack.pop();
                }
            } else {
                let mut parts = tag.split_ascii_whitespace();
                let tag_name = parts.next().unwrap_or("").to_ascii_lowercase();
                if tag_name == "div" {
                    let (mut classes, mut key) = (Vec::new(), None);
                    for (k, v) in attr_iter(tag) {
                        match k.to_ascii_lowercase().as_str() {
                            "class" => {
                                classes = v.split_ascii_whitespace().map(String::from).collect()
                            }
                            "data-key" => {
                                key = Some(v.parse::<u64>().map_err(|_| "data-key 非法")?)
                            }
                            _ => {}
                        }
                    }
                    let Some(key) = key else {
                        return Err("div 缺 data-key（fixture 约定必需）".into());
                    };
                    nodes.push((key, stack.last().copied(), classes, None));
                    stack.push(nodes.len() - 1);
                }
            }
        } else {
            let ch = clean[i..].chars().next().unwrap();
            let mut push = String::new();
            push.push(ch);
            if let Some(rest) = decode_entity(&clean[i..]) {
                push = rest.0;
                i += rest.1;
            } else {
                i += ch.len_utf8();
            }
            text_buf.push_str(&push);
        }
    }
    Ok(nodes)
}

/// 把累积文本折叠空白后并入栈顶节点（white-space:normal 语义）。
fn flush_text(buf: &mut String, nodes: &mut [FixtureNode], stack: &[usize]) {
    if buf.is_empty() || stack.is_empty() {
        buf.clear();
        return;
    }
    let collapsed = collapse_ws(buf);
    buf.clear();
    if collapsed.is_empty() {
        return;
    }
    let top = stack[stack.len() - 1];
    let entry = &mut nodes[top];
    match &mut entry.3 {
        Some(t) => {
            t.push(' ');
            t.push_str(&collapsed);
        }
        None => entry.3 = Some(collapsed),
    }
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("<!--") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 4..];
        match after.find("-->") {
            Some(end) => rest = &after[end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// 属性迭代：扫描 `name="value"`（双引号，值可含空格——class 多词必须完整）。
fn attr_iter(tag: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let bytes = tag.as_bytes();
    let mut i = 0usize;
    // 跳过标签名
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let name_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue; // 无值属性，跳过
        }
        let name = &tag[name_start..i];
        i += 1; // '='
        if i >= bytes.len() || bytes[i] != b'"' {
            continue; // 只支持双引号
        }
        i += 1;
        let val_start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let value = &tag[val_start..i];
        i += 1; // 引号
        out.push((name, value));
    }
    out
}

fn collapse_ws(s: &str) -> String {
    s.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

/// 返回 (解码文本, 消费字节数)；非实体返回 None。
fn decode_entity(s: &str) -> Option<(String, usize)> {
    let rest = s.strip_prefix('&')?;
    let end = rest.find(';')?;
    let name = &rest[..end];
    let decoded = match name {
        "amp" => "&".to_string(),
        "lt" => "<".to_string(),
        "gt" => ">".to_string(),
        "quot" => "\"".to_string(),
        "nbsp" => "\u{00a0}".to_string(),
        other => {
            // 数字实体：#xHH / #HH；前缀不符 → ? 直接返回 None
            if let Some(hex) = other
                .strip_prefix("#x")
                .or_else(|| other.strip_prefix("#X"))
            {
                char::from_u32(u32::from_str_radix(hex, 16).ok()?)?.to_string()
            } else {
                char::from_u32(other.strip_prefix('#')?.parse().ok()?)?.to_string()
            }
        }
    };
    Some((decoded, end + 2))
}
