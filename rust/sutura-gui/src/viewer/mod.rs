pub mod cpu_view;
pub mod wgpu_view;

use glam::{Mat4, Vec3};

use crate::mesh::DisplayMesh;

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    pub target: Vec3,
    pub radius: f32,
}

impl Camera {
    pub fn from_mesh(mesh: &DisplayMesh) -> Self {
        Self {
            yaw: -0.55,
            pitch: 0.32,
            zoom: 1.0,
            target: Vec3::from(mesh.center),
            radius: mesh.radius.max(1e-6),
        }
    }

    fn distance(&self, aspect: f32) -> f32 {
        let half_v = 45f32.to_radians() * 0.5;
        let half_h = (half_v.tan() * aspect.max(0.01)).atan();
        let half = half_v.min(half_h);
        // 12% margin so the bounding sphere never touches the viewport edge.
        (self.radius / half.sin()) * 1.12 * self.zoom
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let dir = Vec3::new(
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
            self.pitch.cos() * self.yaw.cos(),
        );
        let distance = self.distance(aspect);
        let eye = self.target + dir * distance.max(1e-6);
        let near = (distance * 0.01).max(1e-5);
        let far = (distance * 100.0).max(1.0);
        let view = Mat4::look_at_rh(eye, self.target, Vec3::Y);
        let proj = Mat4::perspective_rh(45f32.to_radians(), aspect.max(0.01), near, far);
        proj * view
    }
}

pub enum Viewer {
    Wgpu(wgpu_view::WgpuViewer),
    Cpu(cpu_view::CpuViewer),
}

pub struct Preview {
    pub camera: Camera,
    pub viewer: Viewer,
    pub mesh: Option<std::sync::Arc<DisplayMesh>>,
    pub dirty: bool,
    pub size: [u32; 2],
    pub error: Option<String>,
    pub heatmap: bool,
}

impl Preview {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let force_cpu = std::env::var("SUTURA_GUI_CPU")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let viewer = if force_cpu {
            Viewer::Cpu(cpu_view::CpuViewer::new())
        } else if let Some(state) = cc.wgpu_render_state.clone() {
            match wgpu_view::WgpuViewer::new(state) {
                Ok(v) => Viewer::Wgpu(v),
                Err(e) => {
                    log_line(&format!("wgpu preview unavailable: {e}"));
                    Viewer::Cpu(cpu_view::CpuViewer::new())
                }
            }
        } else {
            log_line("wgpu render state absent; using CPU preview");
            Viewer::Cpu(cpu_view::CpuViewer::new())
        };
        Self {
            camera: Camera {
                yaw: -0.55,
                pitch: 0.32,
                zoom: 1.0,
                target: Vec3::ZERO,
                radius: 1.0,
            },
            viewer,
            mesh: None,
            dirty: true,
            size: [0, 0],
            error: None,
            heatmap: false,
        }
    }

    pub fn set_mesh(&mut self, mesh: std::sync::Arc<DisplayMesh>) {
        self.camera = Camera::from_mesh(&mesh);
        match &mut self.viewer {
            Viewer::Wgpu(v) => v.set_mesh(mesh.clone()),
            Viewer::Cpu(v) => v.set_mesh(mesh.clone()),
        }
        self.mesh = Some(mesh);
        self.dirty = true;
        self.error = None;
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        if response.dragged() {
            let d = response.drag_delta();
            self.camera.yaw -= d.x * 0.008;
            self.camera.pitch = (self.camera.pitch + d.y * 0.008).clamp(-1.45, 1.45);
            self.dirty = true;
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                self.camera.zoom =
                    (self.camera.zoom * (1.0 - scroll * 0.0015)).clamp(0.4, 3.0);
                self.dirty = true;
            }
        }
        let ppp = ui.ctx().pixels_per_point().clamp(1.0, 2.0);
        let size = [
            (rect.width() * ppp).max(8.0) as u32,
            (rect.height() * ppp).max(8.0) as u32,
        ];
        if size != self.size {
            self.size = size;
            self.dirty = true;
        }

        if let Some(mesh) = self.mesh.clone() {
            if self.dirty {
                let aspect = size[0] as f32 / size[1].max(1) as f32;
                let vp = self.camera.view_proj(aspect);
                let result = match &mut self.viewer {
                    Viewer::Wgpu(v) => v.render(&mesh, vp, self.size, self.heatmap),
                    Viewer::Cpu(v) => {
                        v.render(ui.ctx(), &mesh, vp, self.size, self.heatmap)
                    }
                };
                if let Err(e) = result {
                    self.error = Some(e);
                } else {
                    self.error = None;
                }
                self.dirty = false;
            }
            let tex = match &self.viewer {
                Viewer::Wgpu(v) => v.texture_id(),
                Viewer::Cpu(v) => v.texture_id(),
            };
            if let Some(id) = tex {
                ui.painter().image(
                    id,
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            ui.painter().text(
                rect.left_bottom() + egui::vec2(10.0, -10.0),
                egui::Align2::LEFT_BOTTOM,
                rust_i18n::t!("app.triangles", count = mesh.triangle_count()).to_string(),
                egui::FontId::monospace(12.0),
                egui::Color32::from_gray(190),
            );
            if mesh.defect_faces > 0 {
                ui.painter().text(
                    rect.left_top() + egui::vec2(10.0, 10.0),
                    egui::Align2::LEFT_TOP,
                    rust_i18n::t!(
                        "app.defect_faces",
                        count = mesh.defect_faces,
                        pct = format!("{:.1}", mesh.defect_ratio() * 100.0)
                    )
                    .to_string(),
                    egui::FontId::monospace(12.0),
                    egui::Color32::from_rgb(240, 150, 110),
                );
            }
        } else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                rust_i18n::t!("app.drop_hint").to_string(),
                egui::FontId::proportional(16.0),
                egui::Color32::from_gray(160),
            );
        }
        if let Some(e) = &self.error {
            ui.painter().text(
                rect.center_top() + egui::vec2(0.0, 18.0),
                egui::Align2::CENTER_TOP,
                e,
                egui::FontId::monospace(12.0),
                egui::Color32::LIGHT_RED,
            );
        }
    }
}

fn log_line(line: &str) {
    eprintln!("sutura-gui: {line}");
}
