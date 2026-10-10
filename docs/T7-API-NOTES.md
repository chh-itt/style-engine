# T7 winit demo 落地备忘（API 测绘，2026-06；P9-8 增补 wgpu 30 差分，2026-10）

工作区依赖：winit `=0.31.0-beta.3`、wgpu `30.0.1`、vello `0.11.0`（P9-8 前为 wgpu 29.0.4 / vello 0.10.0）。本文是按源码逐个核实过的 API 事实，直接照抄可省一轮轮盘。

## winit 0.31.0-beta.3

- `Window` 已是 **trait**：`event_loop.create_window(attrs)` 返回 `Box<dyn Window>`。
- `ApplicationHandler` 方法（源码 lib.rs:51-64 示例）：
  - `fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop)`（取代 0.30 的 `resumed`）
  - `fn window_event(&mut self, event_loop: &dyn ActiveEventLoop, id: WindowId, event: WindowEvent)`
- 尺寸 API 更名：`with_inner_size` → `with_surface_size`；`window.inner_size()` → `window.surface_size()`（LogicalSize 仍可用）。
- `WindowEvent::Resized` 已不存在（建议放弃 resize 事件，逐帧比对 `surface_size()` 兜底重配 surface）。
- 生命周期方案：`let win: &'static dyn Window = Box::leak(box);` → `&'static dyn Window` 可直接 `create_surface`（若 wgpu 提供 `From<&dyn Window>`；见下）或走 `SurfaceTargetUnsafe`。

## wgpu 30.0.1（P9-8 升级实测差分，相对 29）

- **`RequestAdapterOptions` 新字段 `apply_limit_buckets: bool`**（wgpu-types adapter.rs:56）——填 `false` = 不启用适配器档位桶，等价旧版默认行为。
- **`get_mapped_range()` / `get_mapped_range(..)` 改返回 `Result<BufferView, MapRangeError>`**（原直接返回视图）——读回路径需 `.expect()`（测试）或 `.map_err()`（库）。
- **`SurfaceTexture::present()` 已删除 → `Queue::present(surface_texture)`**（wgpu queue.rs:377，按值消费纹理）。
- **`SurfaceConfiguration` 新字段 `color_space: SurfaceColorSpace`**——`Auto` 为 `#[default]`，文档明言「由后端按格式选择、复现 wgpu 历史行为」；填 `Auto` 即零漂移。
- 其余 29 代事实（下三条）在 30 代仍成立。
- `Instance::new(desc: InstanceDescriptor)` **按值**；`InstanceDescriptor` 无 `Default`，字段：`backends: Backends`、`flags: InstanceFlags`（有 Default）、`memory_budget_thresholds: MemoryBudgetThresholds`（有 Default）、`backend_options: BackendOptions`（有 Default）、`display: Option<Box<dyn …HasDisplayHandle>>` → `None`。
- `DeviceDescriptor` 字段：`label`、`required_features`、`required_limits`、`memory_hints`、`trace: Trace`（device.rs:90，`Trace::Off`）、`experimental_features`（ExperimentalFeatures 在 tokens.rs:9，构造器待查，疑 `disabled()`）。
- `SurfaceConfiguration` 需补 `desired_maximum_frame_latency: 2`。
- `surface.get_current_texture()` 返回枚举 `CurrentSurfaceTexture`：`Success(SurfaceTexture)` / `Suboptimal(SurfaceTexture)` / `Timeout` / `Occluded` / …（非 Result）。
- `From<&dyn winit::window::Window> for SurfaceTarget<'w>` / `From<Box<dyn Window>>` 未能定位确认（pattern 未命中）——写 T7 时先 `cargo check` 一个最小 `create_surface` 试探，不通则改 `create_surface_unsafe` + raw handles。

## vello 0.11.0（0.10 事实在 0.11 全部成立）

- **没有** `render_to_surface`。表面渲染：
  1. `match surface.get_current_texture() { Success(t) | Suboptimal(t) => t, _ => return }`
  2. `let view = tex.texture.create_view(&TextureViewDescriptor::default());`
  3. `renderer.render_to_texture(&device, &queue, &scene, &view, &RenderParams { base_color: peniko::Color, width: u32, height: u32, antialiasing_method: AaConfig })`（lib.rs:473-482，返回 `Result<()>`）
  4. `queue.present(tex);`（wgpu 30：原 `tex.present()` 已迁至 Queue 按值消费）
- `RenderParams`（lib.rs:357）：`base_color` / `width: u32` / `height: u32` / `antialiasing_method: AaConfig`（无 num_threads）。
- `vello::peniko` 与 `vello::kurbo` 均为重导出，勿加直接依赖。

## 已验证可复用的骨架

- 事件循环收尾：`let mut app = DemoApp::default(); let event_loop = EventLoop::builder().build()?; event_loop.run_app(&mut app)?;`（app 先声明，避免 E0597）。
- 引擎侧：`engine.frame((w,h), scale, now)` → `style_engine_vello::render(&frame.paint)` → `Scene`；字体经 `engine.add_font(bytes)` 注入（core crate 不携带字体资产，demo/assets/fonts/ 另有三套测试字体供 demo 使用；未注入字体时文本叶宽高 0 可接受）。

## T5b 关键测绘（parley 0.11 → vello 0.11 字形通路）

- **字体数据直通**：parley `Run::font() -> &FontData`（run.rs:45），`FontData` 即 vello 重导出的 `linebender_resource_handle::FontData`（vello lib.rs:132）——sink 侧排版出的 run 可把字体引用直接交给 vello 绘制，无需二次注册。
- parley `Glyph { id: u32, x: f32, y: f32 }`（glyph.rs:7-10）；`run.glyphs()` / `line.glyphs()`（line.rs:226，绝对坐标待核）均返回 Clone 迭代器。
- vello `DrawGlyphs::draw(style: impl Into<StyleRef>, glyphs: impl Iterator<Item = Glyph>)`（scene.rs:624）——颜色走 `.brush(...)` 设置器、位移走 `.transform(Affine)`（构造器 `DrawGlyphs::new(scene, font)` 参数形待一次编译验证）。
- sink 方案（ADR 一致）：中立 DisplayList 不改；`style-engine-vello` 增加 `VelloTextSystem`（系统字体禁用、宿主推字体、与 core TextSystem 同构），`render_ops` 增带文本形态的重载；demo 把同一份字体字节同时推给 engine（测量）与 sink（绘制）。
- 残留待验证：`DrawGlyphs::new` 形参、行基线 y 的取法（line.rs:179-180 疑为行内绝对坐标字段）。

## 剩余待查（已全部核实，demo 已按此落地）

1. `InstanceDescriptor.display` = `Option<Box<dyn WgpuHasDisplayHandle>>` → `None`。
2. `ExperimentalFeatures::disabled()` 存在（const fn）。
3. `create_surface(&'static dyn Window)` 直接可用（Into<SurfaceTarget<'static>>）。
4. `MemoryBudgetThresholds` 有 `Default`（字段 `for_resource_creation`/`for_device_loss`）。
5. `run_app(self, app: A)` **按值收编**且 `A: ApplicationHandler + 'static`——app 不可用引用传入。
