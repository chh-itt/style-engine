//! 离屏快照（目验通路）：与 demo 同源的场景构建 → vello 离屏渲染 →
//! 纹理回读 → PNG 落盘。运行：`cargo run -p style-engine-demo --example snapshot`。
use style_engine::StyleEngine;
use style_engine::tree::StyleNode;
use style_engine_vello::{VelloTextSystem, render_ops_with_text};

const FONT_REGULAR: &[u8] = include_bytes!("../assets/fonts/DejaVuSans.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf");
// CJK 第二波（Noto Sans SC，OFL 1.1，LICENSE 随目录归档；可变字体默认实例 = Regular）
const FONT_CJK: &[u8] = include_bytes!("../assets/fonts/NotoSansSC.ttf");
const W: u32 = 800;
const H: u32 = 600;

const CSS: &str = "
div.card { box-sizing: border-box; width: 300px; height: 160px; margin: 40px; padding: 16px;
  background-color: #2b6cb0; border-radius: 12px;
  border-style: solid; border-top-width: 2px; border-top-color: #90cdf4; }
div.badge { width: 120px; height: 48px;
  background-image: linear-gradient(90deg, #f6ad55, #f687b3);
  border-radius: 8px; }
div.title { font-family: \"DejaVu Sans\"; font-size: 24px; font-weight: 700; color: #ffffff; }
div.body { font-family: \"DejaVu Sans\"; font-size: 14px; color: #e2e8f0; white-space: normal; }
div.veil { width: 120px; height: 24px; background-color: #ffffff; opacity: 0.55; }
div.cjk { font-family: \"Noto Sans SC\"; font-size: 16px; color: #f1f5f9; white-space: normal; }
div.scroll { width: 240px; height: 80px; overflow-y: scroll; background-color: #0e7490; }
div.row { height: 40px; background-color: #22d3ee; }
div.alt { background-color: #164e63; }
";

fn build() -> (style_engine::Frame<u64>, VelloTextSystem) {
    let mut engine = StyleEngine::new();
    assert!(engine.set_stylesheet(CSS).is_clean());
    engine.add_font(FONT_REGULAR.to_vec());
    engine.add_font(FONT_BOLD.to_vec());
    engine.add_font(FONT_CJK.to_vec());
    let mut text = VelloTextSystem::new();
    text.add_font(FONT_REGULAR.to_vec());
    text.add_font(FONT_BOLD.to_vec());
    text.add_font(FONT_CJK.to_vec());
    let mut node = |parent: Option<u64>, key: u64, cls: &str, txt: Option<&str>| {
        assert!(
            engine
                .insert(
                    parent,
                    key,
                    StyleNode {
                        name: Some("div".into()),
                        classes: vec![cls.into()].into(),
                        text: txt.map(String::from),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
    };
    node(None, 0, "card", None);
    node(Some(0), 1, "badge", None);
    node(Some(0), 2, "title", Some("Style Engine Render Check"));
    node(
        Some(0),
        3,
        "body",
        Some("The quick brown fox jumps over the lazy dog 0123456789"),
    );
    node(Some(0), 4, "veil", None);
    node(
        Some(0),
        9,
        "cjk",
        Some("样式引擎渲染检查：CJK 字体接入与断行。"),
    );
    node(Some(0), 5, "scroll", None);
    node(Some(5), 6, "row", None);
    node(Some(5), 7, "row alt", None);
    node(Some(5), 8, "row", None);
    // ADR-0007：滚动偏移归宿主——快照固定推进 20px 目验平移 + 裁剪
    let _ = engine.set_scroll_offset(5, 0.0, 20.0);
    let frame = engine.frame((W as f32, H as f32), 1.0, 0.0);
    (frame, text)
}

fn main() {
    let (frame, mut text) = build();
    for b in &frame.boxes {
        eprintln!(
            "box key={:?} at=({}, {}) size=({}, {})",
            b.key, b.x, b.y, b.width, b.height
        );
    }
    let mut scene = vello::Scene::new();
    render_ops_with_text(&frame.paint, &mut scene, &mut text);

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("snapshot"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
    }))
    .expect("device");
    let mut renderer =
        vello::Renderer::new(&device, vello::RendererOptions::default()).expect("renderer");
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("snapshot"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // vello 存储纹理路径要求 Rgba8Unorm（非 Srgb 变体）
        format: wgpu::TextureFormat::Rgba8Unorm,
        // vello 存储纹理路径：需 STORAGE_BINDING（其 fine pass 以 storage 绑定目标）
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    renderer
        .render_to_texture(
            &device,
            &queue,
            &scene,
            &view,
            &vello::RenderParams {
                base_color: vello::peniko::color::AlphaColor::new([0.11, 0.12, 0.14, 1.0]),
                width: W,
                height: H,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
        .expect("render to texture");

    // 纹理回读（bytes_per_row 对齐 256）
    let bytes_per_row = ((W * 4 + 255) / 256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(bytes_per_row * H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    {
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .expect("poll");
        rx.recv().unwrap().expect("map async");
    }
    let data = buf.get_mapped_range(..).to_vec();
    buf.unmap();

    let mut img = image::RgbaImage::new(W, H);
    for y in 0..H {
        let start = (y * bytes_per_row) as usize;
        for x in 0..W {
            let o = start + (x * 4) as usize;
            img.put_pixel(
                x,
                y,
                image::Rgba([data[o], data[o + 1], data[o + 2], data[o + 3]]),
            );
        }
    }
    let out = "target/snapshot.png";
    img.save(out).expect("save png");
    println!("saved {out}");
}
