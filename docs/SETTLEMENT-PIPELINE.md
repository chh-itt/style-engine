# 结算管线契约（frame() 调用序列）

> 状态：Accepted（2026-10）。DAG 化（F1）基线——第一步不是重写，是把每处读什么、
> 写什么、依赖什么写清楚。行号以 engine.rs（5977 行）为准，重构后由 ADR-0012 更新。

## 调用序列（engine.rs frame() :762）

```text
frame(viewport, scale, now)
├─ dirty_struct → rebuild_taffy()              # 镜像树 → taffy 树重建
├─ 收敛环 for pass in 0..cap                   # cap = sheet.has_container_rules ? 3 : 1
│  ├─ ① restyle() | restyle_subtrees(roots)    # 样式求解
│  ├─ ② apply_animations()                     # @keyframes 采样覆写
│  ├─ ③ settle_absolute_anchors()              # absolute 重挂包含块
│  ├─ ④ taffy compute_layout(Definite viewport)
│  ├─ ⑤ settle_calc(viewport)                  # 百分比 calc 回写（先于文本重排）
│  ├─ ⑥ settle_tables(viewport)                # 表格列模板（先于文本重排）
│  ├─ ⑦ settle_columns(viewport)               # 多列首次（先于文本重排）
│  ├─ ⑧ [text] T5c-2 文本重排                  # auto_text 叶按包含块内容宽重测
│  │     └─ ⑨ [text] T5d 第三 pass             # absolute shrink-to-fit
│  ├─ ⑩ reflow → 第二次 compute_layout
│  ├─ ⑪ settle_columns(viewport)               # 多列二次平衡
│  └─ ⑫ record_container_sizes()               # 快照；有变且未达 cap → 重入
├─ ⑬ collect()                                 # taffy → 视口绝对坐标
├─ ⑭ settle_column_rules(&layout_by_node)      # 列规条带
├─ ⑮ scrollable 量程（含变换 AABB，仅正向溢出）
└─ ⑯ DisplayList → Frame {boxes, paint, scrollable, generation}
```

## 挂点契约表

| 挂点 | 读 | 写 | 依赖 | 收敛语义 |
|---|---|---|---|---|
| ① restyle | 镜像树、Stylesheet(Epoch)、Environment、StateFlags、容器快照 | styles、min_measures | — | 全量或脏根子树（有容器规则退全量） |
| ② apply_animations | @keyframes、now、底层值 | ComputedStyle 覆写 | ① | 单 pass（now 的纯函数） |
| ③ anchors | positioned/has_transform 谓词、taffy 结构 | set_children 跨父重挂、taffy_parent | ②（动画可翻转 has_transform） | 单 pass（期望子表 diff） |
| ⑤ settle_calc | calc_deferred、父内容盒 | taffy set_style 固定值 | ④ | 上限 3 遍（DAG 逐遍稳一层） |
| ⑥ settle_tables | 首行声明宽、表内容宽 | 行 grid_template_columns | ④⑤ | 上限 2 遍；全等免重排 |
| ⑦⑪ settle_columns | 子件单元高、容器宽 | 幻影列、分配、断口 margin 截断 | ④⑤⑥；⑪ 另依赖 ⑧ | 首次+二次平衡；稳态全等零成本 |
| ⑧ 文本重排 | 包含块内容宽（border-box−padding−有效 border；幻影列 inset=0）、white-space | measures/wrap_widths、set_style、reflow 标 | ⑤⑥⑦ | reflow 单次 |
| ⑨ shrink | min/max 来源=auto_text 探针或 intrinsics；avail=abs_avail_width−自身水平内缩 | 同 ⑧ | ⑧ | 同 pass |
| ⑫ containers | taffy 尺寸−border−padding | container_sizes | ⑪ | 有变→重入（上限 3） |
| ⑬ collect | taffy 布局、幻影补偿、复合变换 | boxes、layout_by_node | 环完成 | — |
| ⑭ column_rules | layout_by_node、styles | ColumnRuleSeg | ⑬ | 单 pass |
| ⑮ scrollable | overflow（解析已归一 auto→Scroll）、后代 border box、复合仿射 | Frame.scrollable | ⑬ | 单 pass |

## 不变量

1. **幂等**：同输入同输出；所有 pass 均 diff-式，稳态零写入零额外布局。
2. **顺序敏感**：⑤⑥⑦→⑧（折行宽依赖结算值）；⑪→⑫（快照取终局布局）；
   ⑬→⑭/⑮（最新鲜布局）。
3. **taffy 是布局唯一真值**：结算只回写样式/结构。
4. **文本重排只动 auto_text**：宿主测量与声明尺寸永不被覆写（第五批⑥）。

## DAG 化（F1）准入基线

「上限 N 遍」换显式依赖求解时：本表即 I/O 契约清单；收敛语义必须保持（稳态零
额外 pass、幂等、顺序约束改依赖驱动）。B 级偏差消除清单：calc 深链 3 遍上限、
rowspan auto 高度分摊、容器查询反向影响退全量。
