use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use egui_wgpu::wgpu;
use egui_wgpu::wgpu::util::DeviceExt;
use glam::Mat4;

use crate::mesh::DisplayMesh;

const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 3],
    normal: [f32; 3],
    defect: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    light: [f32; 4],
    color: [f32; 4],
    heat: [f32; 4],
}

pub struct WgpuViewer {
    state: egui_wgpu::RenderState,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    vertex_buffer: Option<wgpu::Buffer>,
    index_buffer: Option<wgpu::Buffer>,
    index_count: u32,
    color_texture: Option<wgpu::Texture>,
    color_view: Option<wgpu::TextureView>,
    depth_view: Option<wgpu::TextureView>,
    texture_id: Option<egui::TextureId>,
    size: [u32; 2],
}

impl WgpuViewer {
    pub fn new(state: egui_wgpu::RenderState) -> Result<Self, String> {
        let device = &state.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sutura-mesh-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("sutura-mesh-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sutura-mesh-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sutura-mesh-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attrs,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sutura-mesh-uniform"),
            contents: bytemuck::bytes_of(&Uniforms {
                view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                light: [0.4, 0.8, 0.5, 0.0],
                color: [0.24, 0.62, 0.58, 1.0],
                heat: [0.0; 4],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sutura-mesh-bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        Ok(Self {
            state,
            pipeline,
            bind_group,
            uniform,
            vertex_buffer: None,
            index_buffer: None,
            index_count: 0,
            color_texture: None,
            color_view: None,
            depth_view: None,
            texture_id: None,
            size: [0, 0],
        })
    }

    pub fn set_mesh(&mut self, mesh: Arc<DisplayMesh>) {
        let verts: Vec<Vertex> = mesh
            .positions
            .iter()
            .zip(mesh.normals.iter())
            .enumerate()
            .map(|(i, (p, n))| Vertex {
                pos: *p,
                normal: *n,
                defect: mesh.defect.get(i).copied().unwrap_or(0.0),
            })
            .collect();
        self.vertex_buffer = Some(self.state.device.create_buffer_init(
            &wgpu::util::BufferInitDescriptor {
                label: Some("sutura-mesh-verts"),
                contents: bytemuck::cast_slice(&verts),
                usage: wgpu::BufferUsages::VERTEX,
            },
        ));
        self.index_buffer = Some(self.state.device.create_buffer_init(
            &wgpu::util::BufferInitDescriptor {
                label: Some("sutura-mesh-indices"),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            },
        ));
        self.index_count = mesh.indices.len() as u32;
    }

    pub fn texture_id(&self) -> Option<egui::TextureId> {
        self.texture_id
    }

    fn ensure_targets(&mut self, size: [u32; 2]) {
        if self.texture_id.is_some() && self.size == size {
            return;
        }
        let device = &self.state.device;
        if let Some(id) = self.texture_id.take() {
            self.state.renderer.write().free_texture(&id);
        }
        let extent = wgpu::Extent3d {
            width: size[0].max(1),
            height: size[1].max(1),
            depth_or_array_layers: 1,
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sutura-mesh-color"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sutura-mesh-depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let id = self.state.renderer.write().register_native_texture(
            device,
            &color_view,
            wgpu::FilterMode::Linear,
        );
        self.color_texture = Some(color);
        self.color_view = Some(color_view);
        self.depth_view = Some(depth_view);
        self.texture_id = Some(id);
        self.size = size;
    }

    pub fn render(
        &mut self,
        mesh: &DisplayMesh,
        view_proj: Mat4,
        size: [u32; 2],
        heatmap: bool,
    ) -> Result<(), String> {
        if self.index_count == 0 || mesh.indices.len() as u32 != self.index_count {
            self.set_mesh(Arc::new(mesh.clone()));
        }
        self.ensure_targets(size);
        let (Some(color_view), Some(depth_view)) =
            (self.color_view.as_ref(), self.depth_view.as_ref())
        else {
            return Err("wgpu targets unavailable".to_string());
        };
        let aspect = size[0] as f32 / size[1].max(1) as f32;
        let vp = if aspect.is_finite() {
            view_proj
        } else {
            Mat4::IDENTITY
        };
        let uniforms = Uniforms {
            view_proj: vp.to_cols_array_2d(),
            light: [0.45, 0.75, 0.5, 0.0],
            color: [0.24, 0.62, 0.58, 1.0],
            heat: [if heatmap { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
        };
        self.state.queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&uniforms),
        );
        let mut encoder = self
            .state
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sutura-mesh-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sutura-mesh-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.055,
                            g: 0.06,
                            b: 0.07,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            if let (Some(vb), Some(ib)) = (&self.vertex_buffer, &self.index_buffer) {
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..self.index_count, 0, 0..1);
            }
        }
        self.state.queue.submit(Some(encoder.finish()));
        Ok(())
    }
}

const SHADER: &str = r#"
struct Uniforms {
    view_proj: mat4x4<f32>,
    light: vec4<f32>,
    color: vec4<f32>,
    heat: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) defect: f32,
};
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) nrm: vec3<f32>,
    @location(1) defect: f32,
};

@vertex
fn vs(input: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = u.view_proj * vec4<f32>(input.pos, 1.0);
    out.nrm = input.nrm;
    out.defect = input.defect;
    return out;
}

@fragment
fn fs(input: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(input.nrm);
    let l = normalize(u.light.xyz);
    let d = clamp(dot(n, l), 0.0, 1.0);
    let back = clamp(dot(n, -l), 0.0, 1.0) * 0.18;
    let base = u.color.rgb * (0.25 + 0.75 * d) + vec3<f32>(back);
    let hot = vec3<f32>(0.95, 0.28, 0.12) * (0.45 + 0.55 * d);
    let c = mix(base, hot, clamp(input.defect, 0.0, 1.0) * u.heat.x);
    return vec4<f32>(c, 1.0);
}
"#;
