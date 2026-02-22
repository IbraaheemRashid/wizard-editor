use std::sync::{Arc, Mutex};

use egui::{Color32, Rect};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct WaveformUniforms {
    rect_min: [f32; 2],
    rect_max: [f32; 2],
    color: [f32; 4],
    bg_color: [f32; 4],
    screen_size: [f32; 2],
    peak_count: u32,
    _pad: u32,
}

pub struct WaveformRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    max_peaks: usize,
}

impl WaveformRenderer {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("waveform_shader"),
            source: wgpu::ShaderSource::Wgsl(WAVEFORM_SHADER.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("waveform_bind_group_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("waveform_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("waveform_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let max_peaks = 2048;

        Self {
            pipeline,
            bind_group_layout,
            max_peaks,
        }
    }
}

struct WaveformDrawData {
    bind_group: wgpu::BindGroup,
    vertex_count: u32,
}

pub fn waveform_paint_callback(
    rect: Rect,
    peaks: &[(f32, f32)],
    color: Color32,
    bg_color: Color32,
    screen_size: [f32; 2],
) -> egui::PaintCallback {
    let peak_count = peaks.len().min(2048);
    let peaks_data: Vec<[f32; 2]> = peaks
        .iter()
        .take(peak_count)
        .map(|(a, b)| [*a, *b])
        .collect();

    let uniforms = WaveformUniforms {
        rect_min: [rect.min.x, rect.min.y],
        rect_max: [rect.max.x, rect.max.y],
        color: [
            color.r() as f32 / 255.0,
            color.g() as f32 / 255.0,
            color.b() as f32 / 255.0,
            color.a() as f32 / 255.0,
        ],
        bg_color: [
            bg_color.r() as f32 / 255.0,
            bg_color.g() as f32 / 255.0,
            bg_color.b() as f32 / 255.0,
            bg_color.a() as f32 / 255.0,
        ],
        screen_size,
        peak_count: peak_count as u32,
        _pad: 0,
    };

    egui_wgpu::Callback::new_paint_callback(
        rect,
        WaveformCallback {
            uniforms,
            peaks_data,
            peak_count: peak_count as u32,
            draw_data: Arc::new(Mutex::new(None)),
        },
    )
}

struct WaveformCallback {
    uniforms: WaveformUniforms,
    peaks_data: Vec<[f32; 2]>,
    peak_count: u32,
    draw_data: Arc<Mutex<Option<WaveformDrawData>>>,
}

impl egui_wgpu::CallbackTrait for WaveformCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let renderer: &WaveformRenderer = callback_resources
            .get()
            .expect("WaveformRenderer not initialized");

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("waveform_uniforms_per_cb"),
            size: std::mem::size_of::<WaveformUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        queue.write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&self.uniforms));

        let peak_buffer_size = (renderer.max_peaks * std::mem::size_of::<[f32; 2]>()) as u64;
        let peak_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("waveform_peaks_per_cb"),
            size: peak_buffer_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        if !self.peaks_data.is_empty() {
            let byte_data: &[u8] = bytemuck::cast_slice(&self.peaks_data);
            let max_bytes = renderer.max_peaks * std::mem::size_of::<[f32; 2]>();
            let write_bytes = byte_data.len().min(max_bytes);
            queue.write_buffer(&peak_buffer, 0, &byte_data[..write_bytes]);
        }

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("waveform_bind_group"),
            layout: &renderer.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: peak_buffer.as_entire_binding(),
                },
            ],
        });

        *self.draw_data.lock().expect("lock poisoned") = Some(WaveformDrawData {
            bind_group,
            vertex_count: self.peak_count * 6 + 6,
        });

        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        let renderer: &WaveformRenderer = callback_resources.get().expect("WaveformRenderer");
        let guard = self.draw_data.lock().expect("lock poisoned");
        let Some(draw_data) = guard.as_ref() else {
            return;
        };

        if draw_data.vertex_count == 0 {
            return;
        }

        let [sw, sh] = info.screen_size_px;
        render_pass.set_viewport(0.0, 0.0, sw as f32, sh as f32, 0.0, 1.0);

        let clip = info.clip_rect_in_pixels();
        render_pass.set_scissor_rect(
            clip.left_px.max(0) as u32,
            clip.top_px.max(0) as u32,
            (clip.width_px.max(0) as u32).min(sw.saturating_sub(clip.left_px.max(0) as u32)),
            (clip.height_px.max(0) as u32).min(sh.saturating_sub(clip.top_px.max(0) as u32)),
        );

        render_pass.set_pipeline(&renderer.pipeline);
        render_pass.set_bind_group(0, &draw_data.bind_group, &[]);
        render_pass.draw(0..draw_data.vertex_count, 0..1);
    }
}

const WAVEFORM_SHADER: &str = r#"
struct Uniforms {
    rect_min: vec2<f32>,
    rect_max: vec2<f32>,
    color: vec4<f32>,
    bg_color: vec4<f32>,
    screen_size: vec2<f32>,
    peak_count: u32,
    _pad: u32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> peaks: array<vec2<f32>>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;

    if vertex_index < 6u {
        var px: f32;
        var py: f32;
        switch vertex_index {
            case 0u: { px = u.rect_min.x; py = u.rect_min.y; }
            case 1u: { px = u.rect_max.x; py = u.rect_min.y; }
            case 2u: { px = u.rect_min.x; py = u.rect_max.y; }
            case 3u: { px = u.rect_min.x; py = u.rect_max.y; }
            case 4u: { px = u.rect_max.x; py = u.rect_min.y; }
            case 5u: { px = u.rect_max.x; py = u.rect_max.y; }
            default: { px = 0.0; py = 0.0; }
        }
        let ndc_x = (px / u.screen_size.x) * 2.0 - 1.0;
        let ndc_y = 1.0 - (py / u.screen_size.y) * 2.0;
        out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
        out.color = u.bg_color;
        return out;
    }

    let wv_index = vertex_index - 6u;
    let peak_index = wv_index / 6u;
    let corner = wv_index % 6u;

    let rect_w = u.rect_max.x - u.rect_min.x;
    let rect_h = u.rect_max.y - u.rect_min.y;
    let center_y = (u.rect_min.y + u.rect_max.y) * 0.5;
    let half_h = rect_h * 0.4;

    let bar_width = rect_w / f32(u.peak_count);
    let x_left = u.rect_min.x + f32(peak_index) * bar_width;
    let x_right = x_left + bar_width;

    let peak = peaks[peak_index];
    let min_bar_half = 1.0;
    let amp_top = max(max(abs(peak.y), abs(peak.x)), min_bar_half / half_h);
    let amp_bottom = max(abs(peak.x), min_bar_half / half_h);
    let y_top = center_y - amp_top * half_h;
    let y_bottom = center_y + amp_bottom * half_h;

    var px: f32;
    var py: f32;

    switch corner {
        case 0u: { px = x_left; py = y_top; }
        case 1u: { px = x_right; py = y_top; }
        case 2u: { px = x_left; py = y_bottom; }
        case 3u: { px = x_left; py = y_bottom; }
        case 4u: { px = x_right; py = y_top; }
        case 5u: { px = x_right; py = y_bottom; }
        default: { px = 0.0; py = 0.0; }
    }

    let ndc_x = (px / u.screen_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (py / u.screen_size.y) * 2.0;

    out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);

    let amplitude = max(abs(peak.x), abs(peak.y));
    let bright = clamp(amplitude * 2.0, 0.3, 1.0);
    out.color = vec4<f32>(u.color.rgb * bright, u.color.a);

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
"#;
