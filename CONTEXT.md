# style-engine

为 Rust 原生 GUI 提供 CSS 语义样式层的框架无关 crate。它承包"样式声明 → 级联计算 → 布局 → 绘制指令"这一整条链，把窗口、事件、资源等副作用全部留给宿主。

## Language

### 树与身份

**Host**:
拥有窗口、事件循环、应用状态和一切资源副作用的一方（某个框架或某个应用）。
_Avoid_: 宿主应用, framework, embedder

**NodeKey**:
宿主用来标识自己节点的轻量值（泛型参数）。crate 只存储、匹配和回传它，从不解释。
_Avoid_: WidgetId, EntityId, handle

**StyleNode**:
StyleTree 中与一个 NodeKey 对应的镜像条目。
_Avoid_: Widget, Element, component

**StyleTree**:
crate 拥有的样式节点镜像树。文档顺序（children 顺序）即布局顺序与绘制顺序。
_Avoid_: scene graph, DOM

**Viewport**:
宿主每帧提供的布局约束区域与缩放因子。crate 自己不感知窗口。

### 样式输入

**DeclarationBlock**:
一组有序的 CSS 声明。声明顺序（source order）参与级联。
_Avoid_: style struct, props, style map

**Stylesheet**:
解析自真实 CSS 文本的规则集合（选择器 → DeclarationBlock）。

**Origin**:
级联来源：Default（crate 内建默认，扮演 CSS 的 UA 来源）< Stylesheet < Inline（编程式等价 `style=""`）。排序是双键（origin&importance 先排，element-attached 再排）：同 importance 下 Inline 恒胜 Stylesheet；normal 升序 Default < Stylesheet < Inline，important 升序 Stylesheet < Inline < Default（对应 CSS 中 UA !important 最强的无障碍语义）。
_Avoid_: UA sheet, author sheet, user sheet

**StateFlags**:
节点的交互状态位（hover / active / focus / disabled 等），只由宿主推送，crate 从不读取输入设备。
_Avoid_: pseudo state, interaction state

**Clock**:
宿主每帧传入 frame() 的单调绝对时间。crate 没有内部时钟；同输入的两次 frame() 必须产出一致结果。
_Avoid_: dt, delta time, ticker

**Environment**:
宿主推送的环境事实（color-scheme 偏好、reduced-motion、视口尺寸档位），驱动 @media 子集与 light-dark() 的求值。
_Avoid_: media context, settings（Theme 是 Environment + custom properties 的用法，不是机制）

**LeafMeasure**:
宿主为非文本叶子提供的固有尺寸回调（可选）。文本叶子的测量一律由内置文本栈完成。
_Avoid_: TextMeasure trait, measure fn

### 计算与输出

**ComputedStyle**:
声明经级联、继承与单位解析后的最终值集合。
_Avoid_: resolved style, final style, used style（"used value" 专指经布局后的值）

**TextRun**:
文本经 shaping 与断行后的产物，由内置文本栈（parley）生成；DisplayList 以 GlyphRun 消费它。
_Avoid_: glyph cache, text blob, TextMeasure

**LayoutResult**:
节点在 Viewport 内的最终几何：位置、大小、baseline、overflow。
_Avoid_: rect, bounding box（rect 只是其中一项）

**DisplayList**:
绘制指令的有序序列，crate 的中立绘制词汇表，也是唯一允许描述"画什么"的语言。绘制语义的全部出口。
_Avoid_: render list, draw commands, scene

**Frame**:
每帧输出契约：LayoutResult + DisplayList + 失效标记。宿主消费 Frame，不触碰内部状态。
_Avoid_: render output, paint result

**Sink**:
DisplayList 的执行器。vello（GPU）、soft（纯 std 参照）与 tiny（tiny-skia CPU）后端、其他框架的转译器一律是 Sink。
_Avoid_: renderer, backend（renderer 仅指内置 wgpu/vello Sink 的实现细节）

### 失效与验证

**Epoch**:
Stylesheet 的单调版本号。规则变更提升 Epoch，选择器匹配缓存按 Epoch 失效；匹配结果经声明级 diff 无变化则不升级为 style 失效。
_Avoid_: fingerprint, revision, generation

**Invalidation Tier**:
失效的三档粒度 paint < layout < style，单向向上传染。
_Avoid_: dirty flags, dirty bits（dirty bit 只是 tier 的内部实现）

**Conformance Harness**:
以等价 html+css 在浏览器中的输出为基准的自动验证设施，同时充当本 crate 的第一个消费者。
_Avoid_: test suite, golden tests

**Numeric Channel**:
布局一致性对比通道：对同一 NodeKey 比对浏览器 `getBoundingClientRect()` 数值与本方 LayoutResult，0.5px 容差。零栅格化噪声。

**Pixel Channel**:
绘制一致性对比通道：对像素差异做连通域分析并归入 Error Class，按 per-case manifest 的预算判定通过与否。

**Error Class**:
像素差异的显式分类：0 精确匹配 / 1 抗锯齿边缘 / 2 文本栅格化 / 3 几何错误 / 4 颜色混合错误 / 5 基元缺失。3–5 零容忍。
