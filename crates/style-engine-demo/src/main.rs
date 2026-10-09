//! winit demo 宿主（ADR-0006）：宿主推送时钟/环境/样式，帧计算纯函数，
//! DisplayList 经 style-engine-vello 直出 wgpu surface。
//! API 测绘依据 docs/T7-API-NOTES.md。
//!
//! 字体资产内嵌（DejaVu，自由许可、LICENSE 随目录归档）：宿主双推
//! engine 测量 / sink 绘制。滚动为 ADR-0007 的宿主集成样例：滚轮命中
//! 列表盒 → Frame.scrollable 量程夹紧 → set_scroll_offset。

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

const DEMO_CSS: &str = "
div.card { box-sizing: border-box; width: 300px; height: 160px; margin: 40px; padding: 16px;
  background-color: #2b6cb0; border-radius: 12px;
  border-style: solid; border-top-width: 2px; border-top-color: #90cdf4; }
div.badge { width: 120px; height: 48px;
  background-image: linear-gradient(90deg, #f6ad55, #f687b3);
  border-radius: 8px; }
div.title { font-family: \"DejaVu Sans\"; font-size: 24px; font-weight: 700; color: #ffffff; }
div.body { font-family: \"DejaVu Sans\"; font-size: 14px; color: #e2e8f0; white-space: normal; }
div.list { width: 300px; height: 120px; overflow-y: scroll; margin: 0 40px;
  background-color: #0e7490; }
div.row { height: 40px; background-color: #22d3ee; }
div.alt { background-color: #164e63; }
";

// 字体资产（DejaVu，OFL/BSD 类自由许可，LICENSE 随目录归档）：宿主推入。
const FONT_REGULAR: &[u8] = include_bytes!("../assets/fonts/DejaVuSans.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf");

struct GpuState {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: vello::Renderer,
}

struct DemoApp {
    window: Option<&'static dyn Window>,
    gpu: Option<GpuState>,
    engine: StyleEngine<u64>,
    text: style_engine_vello::VelloTextSystem,
    now: f64,
    /// 滚动列表（ADR-0007 宿主集成样例）：宿主拥有的偏移状态。
    scroll_y: f32,
    /// 最近一帧的量程（Frame.scrollable 上报）与列表盒（命中测试用）。
    scrollable_y: f32,
    list_box: Option<(f32, f32, f32, f32)>,
    /// 光标物理坐标（命中测试）。
    cursor: (f64, f64),
}

impl Default for DemoApp {
    fn default() -> Self {
        Self {
            window: None,
            gpu: None,
            engine: StyleEngine::new(),
            text: style_engine_vello::VelloTextSystem::new(),
            now: 0.0,
            scroll_y: 0.0,
            scrollable_y: 0.0,
            list_box: None,
            cursor: (0.0, 0.0),
        }
    }
}

impl DemoApp {
    fn setup_scene(&mut self) {
        debug_assert!(self.engine.set_stylesheet(DEMO_CSS).is_clean());
        // 字体双推（ADR-0006：宿主推字体；engine 测量 + sink 绘制）
        self.engine.add_font(FONT_REGULAR.to_vec());
        self.engine.add_font(FONT_BOLD.to_vec());
        self.text.add_font(FONT_REGULAR.to_vec());
        self.text.add_font(FONT_BOLD.to_vec());
        assert!(
            self.engine
                .insert(
                    None,
                    0,
                    StyleNode {
                        name: Some("div".into()),
                        classes: vec!["card".into()].into(),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(
            self.engine
                .insert(
                    Some(0),
                    1,
                    StyleNode {
                        name: Some("div".into()),
                        classes: vec!["badge".into()].into(),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        // 文本叶（Latin 起步；CJK 字体后续接入）
        assert!(
            self.engine
                .insert(
                    Some(0),
                    2,
                    StyleNode {
                        name: Some("div".into()),
                        classes: vec!["title".into()].into(),
                        text: Some("Style Engine Render Check".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(
            self.engine
                .insert(
                    Some(0),
                    3,
                    StyleNode {
                        name: Some("div".into()),
                        classes: vec!["body".into()].into(),
                        text: Some("The quick brown fox jumps over the lazy dog 0123456789".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        // ADR-0007 滚动列表：5×40 行内容 > 120 盒高 → 量程 80
        assert!(
            self.engine
                .insert(
                    None,
                    4,
                    StyleNode {
                        name: Some("div".into()),
                        classes: vec!["list".into()].into(),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        for (key, class) in [
            (5u64, "row"),
            (6, "row alt"),
            (7, "row"),
            (8, "row alt"),
            (9, "row"),
        ] {
            assert!(
                self.engine
                    .insert(
                        Some(4),
                        key,
                        StyleNode {
                            name: Some("div".into()),
                            classes: vec![class.to_string()].into(),
                            ..Default::default()
                        }
                    )
                    .is_ok()
            );
        }
    }

    fn render(&mut self) {
        let (Some(gpu), Some(window)) = (self.gpu.as_mut(), self.window.as_ref()) else {
            return;
        };
        // 尺寸变化兜底（不依赖 winit resize 事件）
        let size = window.surface_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        if (w, h) != (gpu.config.width, gpu.config.height) {
            gpu.config.width = w;
            gpu.config.height = h;
            gpu.surface.configure(&gpu.device, &gpu.config);
        }
        self.now += 1.0 / 60.0;
        let frame = self
            .engine
            .frame((w as f32, h as f32), window.scale_factor() as f32, self.now);
        // ADR-0007 宿主集成：缓存量程与列表盒（滚轮事件消费，一帧陈旧可接受）
        self.scrollable_y = frame.scrollable.get(&4).map(|s| s.1).unwrap_or(0.0);
        self.list_box = frame.find(4).map(|b| (b.x, b.y, b.width, b.height));
        let mut scene = vello::Scene::new();
        style_engine_vello::render_ops_with_text(&frame.paint, &mut scene, &mut self.text);
        let tex = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            _ => return,
        };
        let view = tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        gpu.renderer
            .render_to_texture(
                &gpu.device,
                &gpu.queue,
                &scene,
                &view,
                &vello::RenderParams {
                    base_color: vello::peniko::color::AlphaColor::new([0.11, 0.12, 0.14, 1.0]),
                    width: w,
                    height: h,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .expect("render to texture");
        // wgpu 30：SurfaceTexture::present() → Queue::present(texture)（消费纹理）。
        gpu.queue.present(tex);
        window.request_redraw(); // 连续帧驱动（演示用）
    }
    /// ADR-0007 宿主集成样例：命中测试 → 量程夹紧 → set_scroll_offset。
    /// 偏移状态与夹紧策略全在宿主——引擎零内部滚动状态。
    fn scroll_list(&mut self, dy: f32) {
        let Some((lx, ly, lw, lh)) = self.list_box else {
            return;
        };
        let (cx, cy) = self.cursor;
        if cx < lx as f64 || cx > (lx + lw) as f64 || cy < ly as f64 || cy > (ly + lh) as f64 {
            return;
        }
        self.scroll_y = clamp_scroll_offset(self.scroll_y, dy, self.scrollable_y);
        let _ = self.engine.set_scroll_offset(4, 0.0, self.scroll_y);
    }
}

/// 滚轮偏移夹紧（纯函数，ADR-0007 宿主职责）：cur − dy 夹到 [0, max]；
/// 边界上重复事件幂等（结果仍贴边界）。
fn clamp_scroll_offset(current: f32, dy: f32, max: f32) -> f32 {
    (current - dy).clamp(0.0, max)
}

impl ApplicationHandler for DemoApp {
    fn can_create_surfaces(&mut self, event_loop: &dyn ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = WindowAttributes::default()
            .with_title("style-engine demo")
            .with_surface_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        let boxed = event_loop.create_window(attrs).expect("create window");
        let window: &'static dyn Window = Box::leak(boxed);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });
        let surface = instance.create_surface(window).expect("create surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            // wgpu 30 新字段；false = 不启用适配器档位桶，等价旧版默认行为。
            apply_limit_buckets: false,
        }))
        .expect("request adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("style-engine-demo"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .expect("request device");
        let size = window.surface_size();
        let caps = surface.get_capabilities(&adapter);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: caps.formats[0],
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            // wgpu 30 新字段；Auto = 由后端按格式选择，复现历史行为。
            color_space: wgpu::SurfaceColorSpace::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        let renderer =
            vello::Renderer::new(&device, vello::RendererOptions::default()).expect("renderer");
        self.gpu = Some(GpuState {
            device,
            queue,
            surface,
            config,
            renderer,
        });
        self.window = Some(window);
        self.setup_scene();
    }

    fn window_event(
        &mut self,
        event_loop: &dyn ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::PointerMoved { position, .. } => {
                self.cursor = (position.x, position.y);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    // winit 0.31：LineDelta 已是 f32（0.30 为 f64）
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y * 40.0,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32,
                    _ => return,
                };
                self.scroll_list(dy);
            }
            _ => {}
        }
    }
}

fn main() {
    let app = DemoApp::default();
    let event_loop = EventLoop::builder().build().expect("event loop");
    // beta.3：run_app(self, app: A) 按值收编（A: ApplicationHandler + 'static）
    event_loop.run_app(app).expect("run app");
}

#[cfg(test)]
mod tests {
    use super::clamp_scroll_offset;

    #[test]
    fn scroll_clamp_is_idempotent_at_bounds() {
        // 顶端上滚：保持 0，重复事件不变
        assert_eq!(clamp_scroll_offset(0.0, 100.0, 80.0), 0.0);
        let once = clamp_scroll_offset(0.0, 100.0, 80.0);
        assert_eq!(clamp_scroll_offset(once, 100.0, 80.0), 0.0);
        // 底端下滚：保持 max
        assert_eq!(clamp_scroll_offset(80.0, -40.0, 80.0), 80.0);
        assert_eq!(clamp_scroll_offset(80.0, -400.0, 80.0), 80.0);
        // 中段正常滚动与远超量程夹紧
        assert_eq!(clamp_scroll_offset(0.0, -30.0, 80.0), 30.0);
        assert_eq!(clamp_scroll_offset(50.0, 20.0, 80.0), 30.0);
        assert_eq!(clamp_scroll_offset(0.0, -400.0, 80.0), 80.0);
        // 量程为 0（无溢出）：任何方向都不动
        assert_eq!(clamp_scroll_offset(0.0, -50.0, 0.0), 0.0);
    }
}
