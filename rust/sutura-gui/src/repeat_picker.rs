// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//! The repeat picker window: two clicks choose a healthy source point and a
//! damaged target point, exactly like `_RepeatPickerView` in `gui.py`.
//!
//! Python renders the mesh with QPainter and reads a face-index pixel; we do the
//! same projection and painter order on the CPU (`sutura_geom::pick`) and draw
//! the flat triangles ourselves. That keeps the picked *value* identical — the
//! gate compares the resulting points, not the pixels.
//!
//! Click order matches Python: the first click sets the source, the second the
//! target, and any further click moves the source (clearing the target).

use sutura_geom::pick::{self, Frame, H, W};

use crate::mesh::DisplayMesh;

/// The picker's state for one mesh.
pub struct RepeatPicker {
    verts: Vec<[f64; 3]>,
    tris: Vec<[i64; 3]>,
    frame: Frame,
    proj: Vec<(f64, f64, f64)>,
    pub source: Option<[f64; 3]>,
    pub target: Option<[f64; 3]>,
}

impl RepeatPicker {
    pub fn new(mesh: &DisplayMesh) -> Self {
        let verts: Vec<[f64; 3]> = mesh
            .positions
            .iter()
            .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
            .collect();
        let tris: Vec<[i64; 3]> = mesh
            .indices
            .chunks_exact(3)
            .map(|c| [c[0] as i64, c[1] as i64, c[2] as i64])
            .collect();
        let frame = pick::shared_frame(&verts, W, H, pick::PAD);
        let proj = verts.iter().map(|v| pick::project(*v, &frame, W, H)).collect();
        Self {
            verts,
            tris,
            frame,
            proj,
            source: None,
            target: None,
        }
    }

    /// Apply a click at pixel `(x, y)`. Returns whether the selection changed.
    pub fn click(&mut self, x: i64, y: i64) -> bool {
        let Some(fid) = pick::face_at(&self.verts, &self.tris, &self.frame, W, H, x, y) else {
            return false;
        };
        let Some(point) = pick::face_centroid(&self.verts, &self.tris, fid) else {
            return false;
        };
        if self.source.is_none() {
            self.source = Some(point);
        } else if self.target.is_none() {
            self.target = Some(point);
        } else {
            self.source = Some(point);
            self.target = None;
        }
        true
    }

    /// Draw the static isometric view and handle clicks.
    ///
    /// The painter's algorithm here is the same order `face_at` uses, so what the
    /// user sees under the cursor is the face that gets picked.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(W as f32, H as f32), egui::Sense::click());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, egui::Color32::from_gray(18));

        if let Some(pos) = response.interact_pointer_pos() {
            if response.clicked() {
                let x = (pos.x - rect.left()).floor() as i64;
                let y = (pos.y - rect.top()).floor() as i64;
                self.click(x, y);
            }
        }

        // Ascending mean depth, so later triangles paint over earlier ones.
        let mut order: Vec<usize> = (0..self.tris.len()).collect();
        let mean_z = |t: &[i64; 3]| {
            (self.proj[t[0] as usize].2 + self.proj[t[1] as usize].2 + self.proj[t[2] as usize].2)
                / 3.0
        };
        order.sort_by(|a, b| {
            mean_z(&self.tris[*a])
                .partial_cmp(&mean_z(&self.tris[*b]))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for i in order {
            let t = self.tris[i];
            let pts: Vec<egui::Pos2> = t
                .iter()
                .map(|vi| {
                    let p = self.proj[*vi as usize];
                    egui::pos2(rect.left() + p.0 as f32, rect.top() + p.1 as f32)
                })
                .collect();
            // Flat grey with a slight depth cue so the form reads.
            let shade = (150.0 - mean_z(&t) * 12.0).clamp(60.0, 220.0) as u8;
            painter.add(egui::Shape::convex_polygon(
                pts,
                egui::Color32::from_gray(shade),
                egui::Stroke::NONE,
            ));
        }

        for (point, color) in [
            (self.source, egui::Color32::from_rgb(46, 204, 113)),
            (self.target, egui::Color32::from_rgb(235, 60, 70)),
        ] {
            if let Some(p) = point {
                let (x, y, _) = pick::project(p, &self.frame, W, H);
                painter.circle_stroke(
                    egui::pos2(rect.left() + x as f32, rect.top() + y as f32),
                    9.0,
                    egui::Stroke::new(3.0, color),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> DisplayMesh {
        let positions = vec![
            [0.0f32, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let tris = [
            [0u32, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        DisplayMesh {
            normals: Vec::new(),
            defect: Vec::new(),
            defect_faces: 0,
            dev_idx: Vec::new(),
            deviation: Vec::new(),
            src_tris: tris.iter().map(|t| [t[0] as i32, t[1] as i32, t[2] as i32]).collect(),
            src_verts: positions.clone(),
            indices: tris.iter().flatten().copied().collect(),
            center: [0.5, 0.5, 0.5],
            radius: 0.87,
            positions,
        }
    }

    /// First click = source, second = target, third moves the source and clears
    /// the target — Python's `mousePressEvent` order.
    #[test]
    fn click_order_matches_python() {
        let mut p = RepeatPicker::new(&cube());
        // These pixels land on three different faces (8, 3, 6), so the third
        // click is a genuinely different point and the move is observable.
        assert!(p.click(320, 240));
        assert!(p.source.is_some());
        assert!(p.target.is_none());
        assert!(p.click(300, 200));
        assert!(p.target.is_some());
        let first = p.source;
        assert!(p.click(250, 250));
        assert_ne!(p.source, first, "third click moves the source");
        assert!(p.target.is_none(), "and clears the target");
    }

    /// A click on background is ignored rather than recorded as a bogus point.
    #[test]
    fn background_click_is_ignored() {
        let mut p = RepeatPicker::new(&cube());
        assert!(!p.click(1, 1));
        assert!(p.source.is_none());
    }
}
