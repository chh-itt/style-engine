# T7 winit demo 落地备忘（API 测绘，2026-06）

工作区依赖：winit `=0.31.0-beta.3`、wgpu `29.0.4`、vello `0.10.0`。本文是按源码逐个核实过的 API 事实，直接照抄可省一轮轮盘。

## winit 0.31.0-beta.3

- `Window` 已是 **trait**：`event_loop.create_window(attrs)` 返回 `Box<dyn Window>`。
- `ApplicationHandler` 方法（源码 lib.rs:51-64 示例）：
  - `fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop)`（取代 0.30 的 `resumed`）
  - `fn window_event(&mut self, event_loop: &dyn ActiveEventLoop, id: WindowId, event: WindowEvent)`
- 尺寸 API 更名：`with_inner_size` → `with_surface_size`；`window.inner_size()` → `window.surface_size()`（LogicalSize 仍可用）。
- `WindowEvent::Resized` 已不存在（建议放弃 resize 事件，逐帧比对 `surface_size()` 兜底重配 surface）。
- 生命周期方案：`let win: &'static dyn Window = Box::leak(box);` → `&'static dyn Window` 可直接 `create_surface`（若 wgpu 提供 `From<&dyn Window>`；见下）或走 `SurfaceTargetUnsafe`。

## wgpu 29.0.4

- `Instance::new(desc: InstanceDescriptor)` **按值**；`InstanceDescriptor` 无 `Default`，字段（wgpu-types instance 区）：`backends: Backends`、`flags: InstanceFlags`（有 Default）、`memory_budget_thresholds: MemoryBudgetThresholds`（instance.rs:325，有 Default）、`backend_options: BackendOptions`（有 Default）、`display: ???`（类型待查，编译错误名单中的缺失字段）。
- `DeviceDescriptor` 字段：`label`、`required_features`、`required_limits`、`memory_hints`、`trace: Trace`（device.rs:90，`Trace::Off`）、`experimental_features`（ExperimentalFeatures 在 tokens.rs:9，构造器待查，疑 `disabled()`）。
- `SurfaceConfiguration` 需补 `desired_maximum_frame_latency: 2`。
- `surface.get_current_texture()` 返回枚举 `CurrentSurfaceTexture`：`Success(SurfaceTexture)` / `Suboptimal(SurfaceTexture)` / `Timeout` / `Occluded` / …（非 Result）。
- `From<&dyn winit::window::Window> for SurfaceTarget<'w>` / `From<Box<dyn Window>>` 未能定位确认（pattern 未命中）——写 T7 时先 `cargo check` 一个最小 `create_surface` 试探，不通则改 `create_surface_unsafe` + raw handles。

## vello 0.10.0

- **没有** `render_to_surface`。表面渲染：
  1. `match surface.get_current_texture() { Success(t) | Suboptimal(t) => t, _ => return }`
  2. `let view = tex.texture.create_view(&TextureViewDescriptor::default());`
  3. `renderer.render_to_texture(&device, &queue, &scene, &view, &RenderParams { base_color: peniko::Color, width: u32, height: u32, antialiasing_method: AaConfig })`（lib.rs:473-482，返回 `Result<()>`）
  4. `tex.present();`
- `RenderParams`（lib.rs:357）：`base_color` / `width: u32` / `height: u32` / `antialiasing_method: AaConfig`（无 num_threads）。
- `vello::peniko` 与 `vello::kurbo` 均为重导出，勿加直接依赖。

## 已验证可复用的骨架

- 事件循环收尾：`let mut app = DemoApp::default(); let event_loop = EventLoop::builder().build()?; event_loop.run_app(&mut app)?;`（app 先声明，避免 E0597）。
- 引擎侧：`engine.frame((w,h), scale, now)` → `style_engine_vello::render(&frame.paint)` → `Scene`；字体经 `engine.add_font(bytes)` 注入（仓库不携带字体资产，文本叶宽高 0 可接受）。

## 剩余待查（已全部核实，demo 已按此落地）

1. `InstanceDescriptor.display` = `Option<Box<dyn WgpuHasDisplayHandle>>` → `None`。
2. `ExperimentalFeatures::disabled()` 存在（const fn）。
3. `create_surface(&'static dyn Window)` 直接可用（Into<SurfaceTarget<'static>>）。
4. `MemoryBudgetThresholds` 有 `Default`（字段 `for_resource_creation`/`for_device_loss`）。
5. `run_app(self, app: A)` **按值收编**且 `A: ApplicationHandler + 'static`——app 不可用引用传入。
