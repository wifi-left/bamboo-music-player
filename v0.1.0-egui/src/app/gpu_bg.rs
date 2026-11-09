//! Real GPU background pipeline for the "GPU blur" and "flow" modes.
//!
//! egui-wgpu lets a widget append a custom render callback to the frame. The
//! callback owns a tiny wgpu pipeline that samples a 160 px cover thumbnail,
//! applies a gaussian blur in the fragment shader and (in flow mode) animates
//! the sampling coordinates. The pipeline lives in egui-wgpu's callback
//! resource map so it is created once per process and shares the frame's
//! device, queue and render pass.

use std::sync::Arc;

use egui::{Color32, ColorImage, Rect};
use egui_wgpu::wgpu;
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor};

const SOURCE_SIZE: u32 = 160;

/// The 160x160 thumbnail that feeds the GPU pipeline.
pub struct BgFrame {
    pub image: ColorImage,
    /// Average colour of the thumbnail, used by the flow mode glow.
    pub average: [f32; 3],
}

impl BgFrame {
    fn new(image: ColorImage) -> Self {
        let mut acc = [0f64; 3];
        let mut count = 0usize;
        for pixel in image.pixels.iter().step_by(7) {
            acc[0] += pixel.r() as f64;
            acc[1] += pixel.g() as f64;
            acc[2] += pixel.b() as f64;
            count += 1;
        }
        let count = count.max(1) as f64;
        // The shader works in linear space; convert the average sRGB colour.
        let average = [0, 1, 2].map(|i| srgb_to_linear((acc[i] / count) as f32 / 255.0));
        Self { image, average }
    }

    /// Synthetic cover for tracks without artwork: a soft diagonal gradient
    /// built from the accent colour so GPU modes still have something to blur.
    fn accent(accent: [u8; 3]) -> Self {
        let size = SOURCE_SIZE as usize;
        let base = [
            accent[0] as f32 / 255.0,
            accent[1] as f32 / 255.0,
            accent[2] as f32 / 255.0,
        ];
        let mut pixels = Vec::with_capacity(size * size);
        for y in 0..size {
            for x in 0..size {
                let t = (x as f32 / size as f32) * 0.45 + (y as f32 / size as f32) * 0.55;
                let dark = 0.16 + 0.30 * t;
                let glow = (1.0
                    - ((x as f32 / size as f32) - 0.68).powi(2)
                    - ((y as f32 / size as f32) - 0.24).powi(2))
                .max(0.0);
                let c = [
                    (base[0] * dark + 0.10 + glow * 0.22).min(1.0),
                    (base[1] * dark + 0.11 + glow * 0.20).min(1.0),
                    (base[2] * dark + 0.15 + glow * 0.30).min(1.0),
                ];
                pixels.push(Color32::from_rgb(
                    (c[0] * 255.0) as u8,
                    (c[1] * 255.0) as u8,
                    (c[2] * 255.0) as u8,
                ));
            }
        }
        Self {
            image: ColorImage::new([size, size], pixels),
            average: [
                srgb_to_linear(base[0]),
                srgb_to_linear(base[1]),
                srgb_to_linear(base[2]),
            ],
        }
    }
}

/// UI-thread state: keeps the current cover thumbnail and the egui texture
/// format. The actual wgpu objects live in the renderer callback resources.
pub struct GpuBackground {
    format: Option<wgpu::TextureFormat>,
    source_key: String,
    source: Option<Arc<BgFrame>>,
    fallback: Option<([u8; 3], Arc<BgFrame>)>,
    version: u64,
    /// Shader time of the last animated frame. Used to freeze the background
    /// in place (instead of snapping to t=0) when animation is paused.
    last_time: f32,
}

impl GpuBackground {
    pub fn new(format: Option<wgpu::TextureFormat>) -> Self {
        Self {
            format,
            source_key: String::new(),
            source: None,
            fallback: None,
            version: 1,
            last_time: 0.0,
        }
    }

    pub fn available(&self) -> bool {
        self.format.is_some()
    }

    /// Replace the source thumbnail. `None` means "this track has no cover";
    /// the accent fallback is used instead.
    pub fn set_source(&mut self, key: impl Into<String>, image: Option<ColorImage>) {
        let key = key.into();
        if self.source_key == key && self.source.is_some() == image.is_some() {
            return;
        }
        self.source_key = key;
        self.source = image.map(|image| Arc::new(BgFrame::new(image)));
        self.version = self.version.wrapping_add(1);
    }

    pub fn clear(&mut self) {
        self.source_key.clear();
        self.source = None;
        self.version = self.version.wrapping_add(1);
    }

    fn frame(&mut self, accent: [u8; 3]) -> Arc<BgFrame> {
        if let Some(frame) = &self.source {
            return frame.clone();
        }
        if let Some((cached, frame)) = &self.fallback
            && *cached == accent
        {
            return frame.clone();
        }
        let frame = Arc::new(BgFrame::accent(accent));
        self.fallback = Some((accent, frame.clone()));
        frame
    }

    /// Append the background callback to the painter. `mode` is 0 for the
    /// static blur and 1 for the animated flow gradient.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        painter: &egui::Painter,
        rect: Rect,
        accent: [u8; 3],
        mode: u32,
        blur: f32,
        opacity: f32,
        dark: bool,
        time: f32,
        animated: bool,
    ) {
        let Some(format) = self.format else {
            return;
        };
        let frame = self.frame(accent);
        let has_cover = self.source.is_some();
        let tint = if dark {
            [0.030, 0.040, 0.065]
        } else {
            [0.930, 0.945, 0.985]
        };
        // Flow mode glows with the cover's own average colour; the accent is
        // only used when there is no artwork.
        let accent_linear = if mode == 1 && has_cover {
            frame.average
        } else {
            [
                srgb_to_linear(accent[0] as f32 / 255.0),
                srgb_to_linear(accent[1] as f32 / 255.0),
                srgb_to_linear(accent[2] as f32 / 255.0),
            ]
        };
        if animated {
            self.last_time = time;
        }
        let time = if animated { time } else { self.last_time };
        let callback = BgCallback {
            frame,
            version: self.version,
            format,
            rect,
            time,
            mode,
            blur,
            opacity,
            dark_tint: tint,
            accent: accent_linear,
        };
        painter.add(egui_wgpu::Callback::new_paint_callback(rect, callback));
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    origin: [f32; 2],
    size: [f32; 2],
    time: f32,
    mode: u32,
    blur: f32,
    opacity: f32,
    manual_srgb: u32,
    _pad0: u32,
    _pad1: [u32; 2],
    tint: [f32; 4],
    accent: [f32; 4],
}

struct BgCallback {
    frame: Arc<BgFrame>,
    version: u64,
    format: wgpu::TextureFormat,
    rect: Rect,
    time: f32,
    mode: u32,
    blur: f32,
    opacity: f32,
    dark_tint: [f32; 3],
    accent: [f32; 3],
}

impl CallbackTrait for BgCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let format = self.format;
        let res = resources
            .entry::<BgResources>()
            .or_insert_with(|| BgResources::new(device, format));

        if res.version != self.version {
            let image = &self.frame.image;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &res.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                image.as_raw(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(SOURCE_SIZE * 4),
                    rows_per_image: Some(SOURCE_SIZE),
                },
                wgpu::Extent3d {
                    width: SOURCE_SIZE,
                    height: SOURCE_SIZE,
                    depth_or_array_layers: 1,
                },
            );
            res.version = self.version;
        }

        let ppp = screen.pixels_per_point;
        let origin = [self.rect.min.x * ppp, self.rect.min.y * ppp];
        let size = [
            (self.rect.width() * ppp).max(1.0),
            (self.rect.height() * ppp).max(1.0),
        ];
        let uniforms = Uniforms {
            origin,
            size,
            time: self.time,
            mode: self.mode,
            blur: self.blur,
            opacity: self.opacity,
            manual_srgb: u32::from(!format.is_srgb()),
            _pad0: 0,
            _pad1: [0; 2],
            tint: [
                srgb_to_linear(self.dark_tint[0]),
                srgb_to_linear(self.dark_tint[1]),
                srgb_to_linear(self.dark_tint[2]),
                1.0,
            ],
            accent: [self.accent[0], self.accent[1], self.accent[2], 1.0],
        };
        queue.write_buffer(&res.uniforms, 0, bytemuck::bytes_of(&uniforms));
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::epaint::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let Some(res) = resources.get::<BgResources>() else {
            return;
        };
        render_pass.set_pipeline(&res.pipeline);
        render_pass.set_bind_group(0, &res.bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

struct BgResources {
    pipeline: wgpu::RenderPipeline,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    version: u64,
}

impl BgResources {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bamboo-bg-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu_bg.wgsl").into()),
        });

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bamboo-bg-uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bamboo-bg-source"),
            size: wgpu::Extent3d {
                width: SOURCE_SIZE,
                height: SOURCE_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bamboo-bg-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bamboo-bg-bind-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bamboo-bg-bind-group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bamboo-bg-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("bamboo-bg-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        Self {
            pipeline,
            texture,
            bind_group,
            uniforms,
            version: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use egui_wgpu::wgpu;

    /// Parses and validates the WGSL source. A GPU adapter is not needed, so
    /// this runs on CI and catches shader edits that would otherwise only fail
    /// at runtime when the first callback is prepared.
    #[test]
    fn background_shader_is_valid() {
        let module = naga::front::wgsl::parse_str(include_str!("gpu_bg.wgsl"))
            .expect("background shader should parse");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .expect("background shader should validate");
    }

    #[test]
    fn uniforms_match_shader_layout() {
        // The shader struct is 80 bytes: vec2+vec2, four f32/u32, two padding
        // u32, then two vec4s. A mismatch here means the CPU writes uniforms at
        // offsets the GPU does not read.
        assert_eq!(std::mem::size_of::<super::Uniforms>(), 80);
    }

    /// Creates the real pipeline on whatever adapter the machine exposes. The
    /// test is skipped when no adapter is available, but on a normal desktop it
    /// catches descriptor/layout mismatches that naga cannot see.
    #[test]
    fn background_pipeline_builds() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: Default::default(),
            backend_options: Default::default(),
            display: None,
        });
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        else {
            return;
        };
        let Ok((device, _queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        else {
            return;
        };
        let _resources = super::BgResources::new(&device, wgpu::TextureFormat::Bgra8UnormSrgb);
    }
}
