use std::sync::Arc;

use glam::{Mat4, Vec3, Vec4};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Transform};

use crate::mesh::DisplayMesh;

const MAX_TRIS: usize = 150_000;

pub struct CpuViewer {
    texture: Option<egui::TextureHandle>,
}

impl CpuViewer {
    pub fn new() -> Self {
        Self { texture: None }
    }

    pub fn set_mesh(&mut self, _mesh: Arc<DisplayMesh>) {}

    pub fn texture_id(&self) -> Option<egui::TextureId> {
        self.texture.as_ref().map(|t| t.id())
    }

    pub fn render(
        &mut self,
        ctx: &egui::Context,
        mesh: &DisplayMesh,
        view_proj: Mat4,
        size: [u32; 2],
        heatmap: bool,
    ) -> Result<(), String> {
        let w = size[0].clamp(16, 1600) as usize;
        let h = size[1].clamp(16, 1200) as usize;
        let mut pixmap = Pixmap::new(w as u32, h as u32)
            .ok_or_else(|| "could not allocate CPU preview".to_string())?;
        pixmap.fill(Color::from_rgba8(14, 15, 18, 255));

        let stride = (mesh.triangle_count() / MAX_TRIS).max(1);
        let light = Vec3::new(0.45, 0.75, 0.5).normalize();
        let mut tris: Vec<(f32, [[f32; 2]; 3], f32, f32)> =
            Vec::with_capacity((mesh.triangle_count() / stride).max(1));
        for (tri_idx, tri) in mesh.indices.chunks_exact(3).enumerate() {
            if tri_idx % stride != 0 {
                continue;
            }
            let mut screen = [[0.0f32; 2]; 3];
            let mut depth = 0.0f32;
            let mut ok = true;
            for (k, &idx) in tri.iter().enumerate() {
                let p = mesh.positions[idx as usize];
                let clip = view_proj * Vec4::new(p[0], p[1], p[2], 1.0);
                if clip.w.abs() < 1e-9 {
                    ok = false;
                    break;
                }
                let ndc = clip.truncate() / clip.w;
                if !ndc.x.is_finite() || !ndc.y.is_finite() {
                    ok = false;
                    break;
                }
                screen[k] = [
                    (ndc.x * 0.5 + 0.5) * w as f32,
                    (1.0 - (ndc.y * 0.5 + 0.5)) * h as f32,
                ];
                depth += ndc.z;
            }
            if !ok {
                continue;
            }
            let n = Vec3::from(mesh.normals[tri[0] as usize]);
            let shade = 0.25 + 0.75 * n.dot(light).max(0.0);
            let defect = tri
                .iter()
                .map(|&i| mesh.defect.get(i as usize).copied().unwrap_or(0.0))
                .fold(0.0f32, f32::max);
            tris.push((depth / 3.0, screen, shade, defect));
        }
        tris.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        for (_, screen, shade, defect) in tris {
            let mut pb = PathBuilder::new();
            pb.move_to(screen[0][0], screen[0][1]);
            pb.line_to(screen[1][0], screen[1][1]);
            pb.line_to(screen[2][0], screen[2][1]);
            pb.close();
            let Some(path) = pb.finish() else { continue };
            let mut paint = Paint::default();
            let (mut r, mut g, mut b) = (61.0 * shade, 158.0 * shade, 148.0 * shade);
            if heatmap && defect > 0.0 {
                let k = defect.clamp(0.0, 1.0);
                let hot = (242.0 * (0.45 + 0.55 * shade), 71.0 * (0.45 + 0.55 * shade), 31.0 * (0.45 + 0.55 * shade));
                r = r * (1.0 - k) + hot.0 * k;
                g = g * (1.0 - k) + hot.1 * k;
                b = b * (1.0 - k) + hot.2 * k;
            }
            paint.set_color_rgba8(
                r.round().clamp(0.0, 255.0) as u8,
                g.round().clamp(0.0, 255.0) as u8,
                b.round().clamp(0.0, 255.0) as u8,
                255,
            );
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }

        let image = egui::ColorImage::from_rgba_premultiplied(
            [w, h],
            pixmap.data(),
        );
        match &mut self.texture {
            Some(handle) => handle.set(image, egui::TextureOptions::LINEAR),
            None => {
                self.texture =
                    Some(ctx.load_texture("sutura-cpu-mesh", image, egui::TextureOptions::LINEAR));
            }
        }
        Ok(())
    }
}
