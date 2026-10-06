# ADR-0034：IFC v2——基线对齐与 vertical-align（settle_lines 升级）

日期：2026-02（P3 批，goal-c293e543）
状态：已接受（落地记录见文末，随实施补记）

## 背景

F1（ADR-0021 D4）IFC 行内流 v1 的行模型：行高 = max(参与者测量高)、
vertical-align = TOP 硬编码（engine.rs `settle_lines` 注释在案）。
文本系统测量（text.rs `measure_rich`）只返回 (宽, 高)，无基线——
基线对齐与 vertical-align 全值无承载数据。

依据：css-inline-3（行盒模型、基线对齐）、css2 §10.8.1
（vertical-align 值族）。

## 事实基线（实施前核查）

- `text.rs:359` `chromium_normal_lh`：已从 parley `Layout::lines()`
  首行首 run 读 `run.metrics().ascent/descent`（fontique typo 值 px）
  ——baseline 提取的现成读取路径，Chromium 对齐 = `round(ascent)`。
- `text.rs:117` `measure_two_pass`：normal 探针缓存
  （族/号/重/斜/caps 键）+ 第二遍 `LineHeight::Absolute` 注入；
  末尾 `(layout.width(), layout.height())`。
- `engine.rs` `settle_lines`：F1 行打包 → Absolute+inset 锚定；
  参与者 = Leaf{文本叶} / Box{inline-block 原子 / inline 组}。
- PropertyId 无 VerticalAlign（P1a 后新增槽位）。

## 决策

### D1：测量层——加法式 `measure_with_baseline`

- 内部 `measure_two_pass` 改产 `(f32, f32, f32)`（宽, 高, 首行基线）：
  `break_all_lines` 后取首行首 run `metrics().ascent.round()`
  （与 ㉔ Chromium 对齐同法整数化；无 run 时 baseline = 0）。
- 新公共方法
  `pub fn measure_with_baseline(&mut self, text, style, spans,
  max_advance, env) -> (f32, f32, f32)`；
  既有 `measure` / `measure_rich` / `measure_min_content` 签名不变
  （消费内部版前两元，零回归面）。

### D2：行模型——基线对齐（css-inline-3 简化行盒）

- 行内参与者两类基线：Leaf = 测量 baseline（D1）；Box = 其内容
  顶 + 首行基线（含文本）或底边（无文本，v1 近似）。
- 行基线 = max(参与者 baseline 距)（以 TOP 装箱后基线最深者定线）；
  行高 = max_asc + max_desc 派生修正——保持 v1「行高 = max(测量高)」
  为下限，基线对齐产生的额外下沉（descender 超出）扩展行盒
  （css-inline-3 行盒增长语义，不做 leading-trim/half-leading 细算）。
- 实现序：v1 的 TOP 装箱不变 → 逐参与者按 vertical-align 计算纵向
  偏移 → inset 锚定值 += 偏移（锚定模型不变，改动局部）。

### D3：vertical-align 属性（新槽位，P1a 后动 property.rs）

- 值族全量：`baseline`(初始) | `sub` | `super` | `text-top` |
  `text-bottom` | `middle` | `top` | `bottom` |
  `<length-percentage>`（percentage 基准 = 行高）。
- 偏移语义（相对行基线）：
  - length/percentage：显式偏移（正=升）；
  - sub/super：固定 em 常量升降——常量值实施时以 Chromium 源码
    常量检索校准；检索不成则取 0.34em 量级兜底，B 级在案
    （规范不固定该值，UA 决定）；
  - middle：基线对齐至 父基线 − x-height/2（x-height 来自
    fontprobe 已注册度量；未注册回退 0.5em，既有近似惯例）；
  - top/bottom：参与者行盒顶/底对齐行顶/底（跳出行基线模型，
    装箱后二次调整）；
  - text-top/text-bottom：参与者字体 asc/desc 对齐父字体度量顶/底。
- 不继承；初始 baseline。

### D4：消费边界

- 仅 `settle_lines` 行内参与者消费（v1 IFC 范围）；taffy flex/grid
  的 baseline 对齐（taffy align-items: baseline）不在本批
  （等上游对齐 API 确认，DEPENDENCIES 阶梯注记）。
- span 级 vertical-align 不做（富文本 span 覆盖=既有近似先例同批）。

## 测试要点（实施时落锁）

- 解析：全值族 + junk 拒绝 + initial=baseline；
- 测量：measure_with_baseline 与 chromium_normal_lh 同源
  （baseline ≈ round(asc)）；
- 行为：`<span style="vertical-align: super">` 上浮（像素/锚点断言）、
  length 偏移精确、top/bottom 对齐行顶底、middle 用 x-height；
- 回归：无 vertical-align 声明时 settle_lines 输出与 v1 逐位一致
  （baseline 值偏移=0）。

## 否决项

- 改 measure_rich 返回签名（破坏面大；加法式新方法）。
- taffy baseline 对齐同步做（上游 API 不熟，独立批）。
- 完整 css-inline-3 行盒（half-leading/leading-trim）——超范围。

## 落地记录

（P3 批实施，全 workspace 559 测试绿；commit 见 CHANGELOG P3 条。）

- **D1**：`text.rs` `measure_two_pass` 末尾改产三元组 `(w, h, baseline)`；
  新私有 `first_run_baseline(&parley::Layout)` = 首行首 run
  `metrics().ascent.round()`（无 run = 0）。公共
  `measure_with_baseline(text, style, spans, max_advance, env) ->
  (f32, f32, f32)`（空文本 (0,0,0)）；`measure` / `measure_rich` /
  `measure_min_content` 签名不变。
- **D3 解析**：`PropertyId::VerticalAlign`（槽位 169，ALL 尾追加，动画
  描述符后移 170..177、SLOT_COUNT=177）；`VerticalAlignKind`
  （non_exhaustive，九变体 + Length(LengthPercentage)）；
  `parse_vertical_align`（八关键字 → 否则 parse_length_percentage）；
  不继承（computed.rs inherits 白名单外）；初始 Baseline；getter
  `ComputedStyle::vertical_align()`（缺槽回退 Baseline）。
- **D2 行结算**：`settle_lines` 两阶段化——TOP 装箱收集改入
  `PendingLinePart`（模块级 struct：tid/va/pb/h/top/font_size/fm）
  → 行结束 `flush_inline_line` 统一结算：L=max(pb)；逐参与者
  vertical-align 求 dy 回填 `inset.top += dy`；行盒扩展
  `final_h = max(装箱行高, max(dy+h))`；bottom 二遍 `dy=final_h−h`。
  flush 挂点两处：Box 换行判定处 + run 循环尾。Box 基线探针
  `box_first_text_baseline`：DFS 子树首个文本叶（taffy location.y
  沿路径累加 + 叶 measure_with_baseline 基线；无文本 = 盒高）。
- **字体度量扩展**：`fontprobe::RawMetrics` + `ascent_per_em`/
  `descent_per_em`（hhea int16 @4/@6，abs 归一恒正；缺失回退
  0.8/0.2）→ `value::FontMetrics` 同名字段 → add_font 注入链——
  text-top/text-bottom/middle 的 strut 消费面。

### 实施偏差（相对决策原文，B 级在案）

1. **baseline（初始值）不产生偏移**（dy=0，TOP 装箱原位）：若
   baseline 亦对齐 L=max(pb)，全部 baseline 参与者会随最深基线整体
   平移，破坏 v1 逐位一致回归锁（ADR-0021 F1 契约）；单字号同字体
   场景两者等价，混字号默认下沉留待后续（显式值语义不受影响）。
2. **sub/super = ±0.34em 常量**：Chromium 源码常量检索未做，兜底
   值在案（决策 D3 预授权路径）。
3. **middle 的 x-height** = 既有 fontprobe `ex_per_em`（x 字形
   yMax 探针），非新探测；未注册走 FontMetrics 默认 0.5em。
4. **% 基准** = 装箱行高（TOP 装箱后的 line_h，非最终扩展行高）；
   calc() 承载偏移 = 0。
5. **text-top/text-bottom** strut = 容器（行属主）ComputedStyle 的
   font_metrics/font_size；参与者侧用自身 font_metrics。
6. **行盒扩展**简化为 max(参与者盒底)（含偏移），不做 half-leading
   分摊（决策 D2 已声明不做 leading 细算）。

### 测试（8 项）

- `decl.rs::vertical_align_parse_family`：全值族 12 例（% 存小数/
  负值/em）+ 拒绝 4 例；
- `text.rs::measure_with_baseline_first_run_ascent`：空 (0,0,0)、
  0<b<h、32px/16px 基线线性≈2；
- `engine.rs` 行为锁 6 项：`vertical_align_no_decl_bitwise_v1`
  （无声明 vs 显式 baseline 逐位一致）、`…_length_offset`
  （va:10px 盒 dy=−10 精确）、`…_super_sub_shifts`（±5.44px +
  行盒扩展进容器高）、`…_middle_between_super_and_baseline`
  （方向序）、`…_box_baseline_from_text`（盒基线=子文本基线，
  区分盒高误用）、`…_top_bottom_alignment`（top 即位/bottom
  二遍 =20px）。
