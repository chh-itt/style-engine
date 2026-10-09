# ADR-0042: span 级行高与字距（ranged push 收口）

- 状态：已接受（2026-10，P9-5）
- 关联：FEATURES.md 文本条 / span 级深化条；DEPENDENCIES.md（parley 0.11.1 调研）；P7 span 富文本基线（span 色/字号/族/字重/斜体）

## 背景

P7 落地富文本 span 时，span 覆盖样式中的 `line-height` 与 `letter-spacing`
仅为「基样式生效」的已知近似（B 级在案）：span 区间上的行高/字距声明解析
入库，但测量与绘制通路只推送节点基样式值，span 覆盖被静默忽略。同级先例
（stretch/word-spacing/features/variations）记录于 FEATURES.md 偏差核对节。
1.0 对齐要求消除可在当前架构内正确表达的偏差。

## parley 0.11.1 事实约束

- `LineHeight` 枚举 = `MetricsRelative(f32) | FontSizeRelative(f32) |
  Absolute(f32)`，**无 Normal 变体**（默认 `MetricsRelative(1.0)`）。显式
  `line-height: normal` 无法在 parley 中表达为「字体度量 normal」——它与
  引擎基样式的 Normal 哨兵语义不等价。
- 行盒行高 = 行内各 run 解析值的 running max（`line_break.rs:242
  add_line_height`）——与 CSS 行盒模型（max of span leading）同向，ranged
  push 即可让覆盖 span 抬升整行行高。
- `LetterSpacing` 按簇 ranged 生效；ranged push 的后推覆盖前推（与 span
  字号/字重同一语义）。

## 决策

- **D1 通路**：span 覆盖样式在 `build_layout`（text.rs）span 循环内经
  parley ranged push 生效——测量与 vello 绘制同一规则，保证折行一致：
  - 行高：仅当 span 计算值 ≠ `LineHeight::Normal` 且解析出 px 时 push
    ranged `Absolute(lh_px)`；显式 normal 不推（回退基样式值）。
  - 字距：**恒推** ranged `LetterSpacing`——显式 `0` 覆盖继承的非零基值是
    精确语义（若仅在非零时推，span 的 0 会被基值淹没）。
- **D2 绘制契约**：`PaintOp::Text` 的 `TextSpanPaint` 增加
  `letter_spacing: f32` 与 `line_height: Option<f32>`（None = normal，回退
  op 级 line_height）；两发射点（主 spans 与 caps 合成 spans）均填充；serde
  dump 同步（旧 dump 缺字段经 `#[serde(default)]` 兼容）。
- **D3 soft sink**：`text_device_polys` 逐字符字距取 span 覆盖值（与基值
  二选一）；span 行高**不消费**——soft 为单行渲染、无行盒语义（B·豁免，
  重估条件=soft 承载多行富文本排版）。
- **D4 normal 回退近似（B 级）**：span 显式 `line-height: normal` 回退基
  样式默认（如 20px）而非字体度量 normal（Chromium ≈16px @16px 字号）。
  在案重估条件：parley 提供 Normal 语义变体。测试以锁的形式固化该回退
  （`span_line_height_grows_line_box` 第三段）。
- **D5 否决替代**：
  - 仅在 sink 侧消费 span 行高/字距、测量保持基值——破坏「测量与绘制同
    源」契约（折行不一致），否决。
  - 引擎侧预折行展开 span 行高为半 leading 偏移——行盒模型重构，IFC T-契
    约邻域，工程量与收益不匹配，否决（defer 行盒级正确性至 IFC 重构）。

## 影响

- span 覆盖行高/字距从「静默忽略」升级为「测量+vello 绘制精确生效、soft
  字距生效」；偏差面收窄至两项文档化豁免（normal 回退、soft 行高）。
- `TextSpanPaint` 公共结构新增两字段——引擎未发布（0.1.0、不发布
  crates.io），无 semver 影响；serde dump 版本兼容（缺字段默认值）。
- 锁定测试：`tests/css_text.rs` 3 件（span 字距加宽测量+绘制终结值、span 0
  覆盖继承基值、span 行高主导行盒+normal 回退锁）。
