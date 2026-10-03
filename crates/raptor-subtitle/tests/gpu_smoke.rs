//! 无头 GPU 冒烟测试：验证字幕 overlay 管线（含 alpha uniform）真正可渲染，
//! 以及 \move 定位与 \fad 淡入淡出在像素层面的效果。
//! 无可用 GPU / 字体时软跳过。

use raptor_render::Overlay;
use raptor_subtitle::{load_system_font, AssEvent, AssParser, SubtitleConfig, SubtitleEngine};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const W: u32 = 320;
const H: u32 = 240;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn try_gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: Default::default(),
        backend_options: Default::default(),
        display: None,
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;
    let usages = adapter.get_texture_format_features(FORMAT).allowed_usages;
    if !usages.contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        || !usages.contains(wgpu::TextureUsages::COPY_SRC)
    {
        return None;
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("subtitle_smoke_device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: Default::default(),
        memory_hints: Default::default(),
        trace: Default::default(),
    }))
    .ok()?;
    Some(Gpu { device, queue })
}

fn engine_with_ass(src: &str) -> Option<SubtitleEngine> {
    let mut engine = SubtitleEngine::new(SubtitleConfig {
        canvas_width: W,
        canvas_height: H,
        ..Default::default()
    });
    let font = load_system_font()?;
    engine.set_font(font);
    let doc = AssParser::new().parse_document(src.as_bytes());
    {
        let shared = engine.shared_state();
        let mut st = shared.lock();
        st.events = doc.events.iter().map(|e| e.base.clone()).collect();
        st.ass_events = doc.events;
        st.styles = doc.styles;
        st.play_res = (doc.play_res_x, doc.play_res_y);
    }
    Some(engine)
}

/// 渲染一帧到离屏纹理，返回 (非零 alpha 像素数, 最左非零像素 x 坐标)
fn render_frame(engine: &mut SubtitleEngine, gpu: &Gpu, pts: f64) -> (usize, Option<u32>) {
    engine.update(pts);
    let dbg_text = engine.shared_state().lock().current_text.clone();
    assert!(
        !dbg_text.is_empty() || pts > 5.5,
        "no active items at pts={pts}"
    );

    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("smoke_target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("smoke_encoder"),
        });
    engine.render(&gpu.device, &gpu.queue, &mut encoder, &view, FORMAT, W, H);

    let bytes_per_row = W * 4; // 1280，满足 256 对齐
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("smoke_readback"),
        size: u64::from(bytes_per_row) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(std::iter::once(encoder.finish()));

    let slice = buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = sender.send(r);
    });
    let _ = gpu.device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    });
    receiver.recv().unwrap().expect("map readback");
    let data = slice.get_mapped_range();

    let mut painted = 0usize;
    let mut min_x: Option<u32> = None;
    for row in data.chunks(bytes_per_row as usize) {
        for (x, px) in row.chunks(4).enumerate() {
            if px[3] > 0 {
                painted += 1;
                min_x = Some(min_x.map_or(x as u32, |m| m.min(x as u32)));
            }
        }
    }
    drop(data);
    buffer.unmap();
    (painted, min_x)
}

#[test]
fn gpu_smoke_static_and_inactive() {
    let Some(gpu) = try_gpu() else { return };
    let Some(mut engine) = engine_with_ass(
        r#"[Script Info]
PlayResX: 320
PlayResY: 240

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,字幕测试
"#,
    ) else {
        return;
    };
    let (painted, _) = render_frame(&mut engine, &gpu, 2.0);
    assert!(painted > 50, "expected glyph pixels, got {painted}");
    let (painted, _) = render_frame(&mut engine, &gpu, 6.0);
    assert_eq!(painted, 0, "inactive subtitle must not paint");
}

#[test]
fn gpu_smoke_move_shifts_pixels_right() {
    let Some(gpu) = try_gpu() else { return };
    let Some(mut engine) = engine_with_ass(
        r#"[Script Info]
PlayResX: 320
PlayResY: 240

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:00.00,0:00:10.00,Default,,0,0,0,,{\an7\move(10,100,250,100)}i
"#,
    ) else {
        return;
    };
    let (_, left_early) = render_frame(&mut engine, &gpu, 0.5);
    let (_, left_late) = render_frame(&mut engine, &gpu, 9.0);
    assert!(
        left_early.is_some() && left_late.is_some(),
        "both frames must paint the glyph"
    );
    assert!(
        left_late > left_early,
        "moved glyph must shift right: {left_early:?} -> {left_late:?}"
    );
}

#[test]
fn gpu_smoke_fade_in_visible_after_invisible() {
    let Some(gpu) = try_gpu() else { return };
    let Some(mut engine) = engine_with_ass(
        r#"[Script Info]
PlayResX: 320
PlayResY: 240

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:05.00,Default,,0,0,0,,{\fad(2000,0)}fade
"#,
    ) else {
        return;
    };
    // t=start：alpha=0 → 完全不可见
    let (painted, _) = render_frame(&mut engine, &gpu, 1.0);
    assert_eq!(painted, 0, "alpha 0 must not paint");
    // 淡入中段：可见
    let (painted, _) = render_frame(&mut engine, &gpu, 2.0);
    assert!(painted > 50, "faded-in text must paint, got {painted}");
}

/// AssEvent 段折叠不依赖样式表缺失崩溃
#[test]
fn missing_style_falls_back_without_panic() {
    let src = r#"[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:05.00,Nonexistent,,0,0,0,,{\an5}orphan style
"#;
    let doc = AssParser::new().parse_document(src.as_bytes());
    assert!(doc.styles.is_empty());
    let ev: &AssEvent = &doc.events[0];
    assert_eq!(ev.base.text, "orphan style");
}
