// Copyright (C) 2026 Sutura Authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// extern "C" shim around the vendored standalone Geogram Delaunay PSM
// (third_party/geogram_delaunay_psm/Delaunay_psm.{h,cpp}, generated from the
// Geogram distribution with tools/extract_psm.sh).
//
// Points in -> finite tetrahedra + per-facet neighbour cells out.
// Uses the sequential GEO::Delaunay3d engine (not ParallelDelaunay3d), so the
// result is single-threaded and deterministic.

#include "Delaunay_psm.h"

#include <cstdio>

namespace {
void geo_psm_init() {
    static bool done = false;
    if (done) {
        return;
    }
    GEO::initialize();
    GEO::Logger::instance()->set_quiet(true);
    done = true;
}
}  // namespace

extern "C" {

// 3D Delaunay tetrahedralization of n points.
//   pts        : n*3 doubles (row-major x y z)
//   tets       : output, 4 ints per finite cell (indices into pts)
//   neigh      : output, 4 ints per finite cell; neighbour across the face
//                opposite local vertex lv, or -1 when that face is on the
//                convex hull / belongs to an infinite cell
//   max_tets   : capacity of tets/neigh in cells
//   out_n      : number of finite cells
//   out_nb_vert: number of vertices retained by the engine
// Returns 0 ok, 2 if the buffer is too small (out_n set to the required
// count), 3 on an exception.
int geo_psm_delaunay3(const double* pts, long long n, int* tets, int* neigh,
                      long long max_tets, long long* out_n,
                      long long* out_nb_vert) {
    try {
        geo_psm_init();
        // The PSM public header exposes the Delaunay base + factory only;
        // "BDEL" resolves to the sequential GEO::Delaunay3d (not the parallel
        // "PDEL"), registered by GEO::Delaunay::initialize().
        GEO::Delaunay_var d = GEO::Delaunay::create(3, "BDEL");
        d->set_keeps_infinite(true);
        d->set_stores_neighbors(true);
        d->set_vertices((GEO::index_t)n, pts);
        const GEO::index_t nf = d->nb_finite_cells();
        *out_n = (long long)nf;
        *out_nb_vert = (long long)d->nb_vertices();
        if ((long long)nf > max_tets) {
            return 2;
        }
        for (GEO::index_t c = 0; c < nf; ++c) {
            for (int lv = 0; lv < 4; ++lv) {
                tets[(long long)c * 4 + lv] =
                    (int)d->cell_vertex(c, (GEO::index_t)lv);
            }
            for (int lf = 0; lf < 4; ++lf) {
                const GEO::index_t a = d->cell_adjacent(c, (GEO::index_t)lf);
                neigh[(long long)c * 4 + lf] =
                    (a == GEO::NO_INDEX || a >= nf) ? -1 : (int)a;
            }
        }
        return 0;
    } catch (const std::exception& e) {
        std::fprintf(stderr, "geo_psm exception: %s\n", e.what());
        return 3;
    } catch (...) {
        return 3;
    }
}

}  // extern "C"
