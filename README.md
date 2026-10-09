# style-engine

**为 Rust 原生 GUI 提供的框架无关 CSS 语义层**：承包「样式声明 → 级联计算 → 布局 → 绘制指令」整条链，窗口/事件/时钟/资源等副作用全部留给宿主。

- **L1 样式代数**：真实 CSS 文本解析（cssparser）+ 选择器匹配（selectors）+ 自研属性文法，产出 `ComputedStyle`（183 属性槽位 + custom properties）。
- **L2 布局**（feature = `layout`）：taffy（flex/grid/block/absolute）+ 自研结算层（calc 17 轴结算式、表格两阶段列宽、多列二分平衡）+ 内置文本栈 parley（测量/断行/bidi）。
- **L3 绘制**：中立 `DisplayList`（`PaintOp` 指令序列）——渲染由宿主接入任意后端；参考实现 `style-engine-vello`（GPU），确定性参照 `style-engine-soft`（零依赖纯软光栅）。

**定位**：桌面 GUI 的 CSS 语义层（面向 egui/iced/bevy 类宿主）；以浏览器为**度量衡**而非目标——正确性由 conformance harness 对比 Chromium golden（Numeric 0.5px 容差 + Pixel 错误分类双通道，当前 35 用例零 xfail）证明。子集边界由 [docs/FEATURES.md](docs/FEATURES.md) 权威定义：每行特性要么有金标准证据，要么有显式排除理由与重估条件。

## 快速上手

安装（crates.io）：

```powershell
cargo add style-engine style-engine-vello
```

```rust
use style_engine::{StyleEngine, StyleNode};

let mut engine = StyleEngine::new();
engine.set_stylesheet(".btn { background-color: #3366cc; padding: 8px 16px; }");

// 宿主树镜像：根 1 → 按钮 2（K 为宿主自己的 Copy + Eq + Hash 键）
engine.insert(None, 1, StyleNode::default())?;
let mut btn = StyleNode::default();
btn.name = Some("button".into());
btn.classes = ["btn".to_string()].into_iter().collect();
engine.insert(Some(1), 2, btn)?;

// 每帧：视口 + 缩放因子 + 单调时钟（crate 无内部时钟，同输入同输出）
let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
let hit = frame.find(2).expect("node laid out");

// 绘制：DisplayList 交给任意 Sink
let scene = style_engine_vello::render(&frame.paint);
```

零副作用契约：字体/图片字节由宿主推入（`add_font` / `add_image`），交互状态由宿主推送（`set_state`），滚动偏移归宿主（`set_scroll_offset`，量程经 `Frame.scrollable` 上报）。错误双轨：CSS 内容错误按规范容错进 `ParseReport`；宿主违约以 `ContractError` 上浮。

## Workspace 布局

| crate | 职责 |
|---|---|
| `style-engine` | 核心：L1/L2/L3，无 GPU 依赖 |
| `style-engine-vello` | GPU Sink 参考实现（wgpu/vello 只在此） |
| `style-engine-soft` | 纯软件光栅 Sink（零第三方运行时依赖，像素确定性） |
| `style-engine-conformance` | Chromium golden 双通道对比 harness |
| `style-engine-demo` | winit 敌意消费者演示 |

## Feature 门禁

- `layout`（默认开）：taffy 布局与全部结算 pass
- `text`（默认开）：parley 文本测量/断行
- `serde`：DisplayList/ComputedStyle 机器通道（paint_dump OpDump 镜像、
  调试 dump 往返锁）
- `--no-default-features`：仅 CSS 解析/级联/绘制编译

## 文档索引

- [CONTEXT.md](CONTEXT.md) — 领域语言（术语表）
- [docs/V1-SCOPE.md](docs/V1-SCOPE.md) — v1.0 行为验收基准（承诺面/偏差面/排除面）
- [docs/adr/](docs/adr/) — 架构决策记录（DisplayList 中立契约、渲染栈、conformance、推送式同步、滚动、层叠、transform、…）
- [docs/FEATURES.md](docs/FEATURES.md) — 特性注册表（子集边界权威 + 偏差分级 A/B/C）
- [docs/PERFORMANCE.md](docs/PERFORMANCE.md) — 性能预算与门禁推导
- [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md) — 依赖政策与版本地板链
- [CHANGELOG.md](CHANGELOG.md) — 按阶段演进记录

## 本地开发

```powershell
.\run.ps1            # fmt + clippy + test --all-features + hack 幂集（+ perf）
cargo test -p style-engine --all-features   # 仅核心测试
```

MSRV **1.90**（edition 2024；依赖地板链见 DEPENDENCIES.md）。许可 MIT OR Apache-2.0；发布 crates.io（2026-10 决策，废止早期「不发布」约定）。
