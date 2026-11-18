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
        self.render_with(ctx, mesh, view_proj, size, heatmap, false)
    }

    pub fn render_with(
        &mut self,
        ctx: &egui::Context,
        mesh: &DisplayMesh,
        view_proj: Mat4,
        size: [u32; 2],
        heatmap: bool,
        defect_colors: bool,
    ) -> Result<(), String> {
        let pixmap = self.rasterize_with(mesh, view_proj, size, heatmap, defect_colors)?;
        let image = egui::ColorImage::from_rgba_premultiplied(
            [pixmap.width() as usize, pixmap.height() as usize],
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

    /// Draw the mesh into a pixmap without touching the texture.
    ///
    /// Split out of `render` so the static before/after view can rasterise the
    /// same way the live preview does -- one renderer, so the two cannot show
    /// different pictures of the same mesh.
    pub fn rasterize(
        &self,
        mesh: &DisplayMesh,
        view_proj: Mat4,
        size: [u32; 2],
        heatmap: bool,
    ) -> Result<Pixmap, String> {
        self.rasterize_with(mesh, view_proj, size, heatmap, false)
    }

    pub fn rasterize_with(
        &self,
        mesh: &DisplayMesh,
        view_proj: Mat4,
        size: [u32; 2],
        heatmap: bool,
        defect_colors: bool,
    ) -> Result<Pixmap, String> {

        let w = size[0].clamp(16, 1600) as usize;
        let h = size[1].clamp(16, 1200) as usize;
        let mut pixmap = Pixmap::new(w as u32, h as u32)
            .ok_or_else(|| "could not allocate CPU preview".to_string())?;
        pixmap.fill(Color::from_rgba8(14, 15, 18, 255));

        let stride = (mesh.triangle_count() / MAX_TRIS).max(1);
        let light = Vec3::new(0.45, 0.75, 0.5).normalize();
        // (depth, screen, shade, defect weight, deviation-ramp index)
        // The face id is carried so the painter can look up the defect-kind
        // colour; `dev_idx` is already indexed by it.
        let mut tris: Vec<(f32, [[f32; 2]; 3], f32, f32, Option<u16>, usize)> =
            Vec::with_capacity((mesh.triangle_count() / stride).max(1));
        let dev_lut = sutura_geom::heatmap::deviation_lut();
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
            // The ramp is indexed by the ORIGINAL face id, which is the soup's
            // face id because the soup is built one triangle at a time in order.
            let dev = mesh.dev_idx.get(tri_idx).copied();
            tris.push((depth / 3.0, screen, shade, defect, dev, tri_idx));
        }
        tris.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        for (_, screen, shade, defect, dev, tri_idx) in tris {
            let mut pb = PathBuilder::new();
            pb.move_to(screen[0][0], screen[0][1]);
            pb.line_to(screen[1][0], screen[1][1]);
            pb.line_to(screen[2][0], screen[2][1]);
            pb.close();
            let Some(path) = pb.finish() else { continue };
            let mut paint = Paint::default();
            let (mut r, mut g, mut b) = (61.0 * shade, 158.0 * shade, 148.0 * shade);
            // Same precedence as `heatmap.draw_frame`: an available deviation
            // ramp wins over the defect blend, which wins over neutral grey.
            // Defect-kind colour wins over everything: Python's `defects` view
            // is a mode of its own, not an overlay on the deviation ramp.
            let kind = if defect_colors {
                mesh.defect_colors
                    .get(tri_idx)
                    .copied()
                    .flatten()
            } else {
                None
            };
            let ramp = if heatmap { dev } else { None };
            if let Some(rgb) = kind {
                r = rgb[0] as f32;
                g = rgb[1] as f32;
                b = rgb[2] as f32;
            } else if let Some(idx) = ramp {
                // Shaded by the same lambert term as the neutral body. The ramp
                // alone maps every face of a scanned part to nearly the same
                // deviation, so an unshaded ramp is one flat silhouette and the
                // shape cannot be read at all -- that is what the After +
                // Heatmap view looked like on a Steam Deck. Multiplying by
                // `shade` keeps the ramp's own values (the legend still maps to
                // the same colours) and only modulates their brightness, the
                // standard way to keep a colour map legible.
                // A gentler lambert term than the body's: the ramp's low end is
                // navy `[10, 20, 80]`, and multiplying it by the body's 0.25
                // floor gives `[3, 5, 20]` -- black, with the shape still
                // unreadable and the colour meaningless. Keeping 55% ambient
                // leaves the legend's colours recognisable while the remaining
                // 45% still varies with the normal, so the form reads.
                let ramp_shade = 0.75 + 0.25 * shade;
                let c = dev_lut[idx as usize % dev_lut.len()];
                r = c[0] as f32 * ramp_shade;
                g = c[1] as f32 * ramp_shade;
                b = c[2] as f32 * ramp_shade;
            } else if heatmap && defect > 0.0 {
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
        Ok(pixmap)
    }
}

