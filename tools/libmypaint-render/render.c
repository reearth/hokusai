// libmypaint-render: drive upstream libmypaint with a hokusai-compat script
// and emit a raw RGBA8 buffer (composited over white, sRGB-encoded) on stdout.
//
// Usage:
//   libmypaint-render <script.json> <brush.myb>
// Stdout: width*height*4 bytes of RGBA8. Width/height come from the script.
//
// We drive libmypaint via `mypaint_brush_stroke_to_2`, the non-legacy API
// that respects each brush's `paint_mode` setting (legacy `stroke_to`
// silently forces paint_mode to 0, so blender / smudge / watercolour
// brushes wouldn't compare against hokusai's spectral pigment blend).
// Since the bundled `MyPaintFixedTiledSurface` only exposes a Surface 1
// interface, we wire up our own minimal `MyPaintTiledSurface2` backed by
// a flat tile grid for the canvas extent given by the script.
//
// The flatten path mirrors hokusai_compat::render so the C and Rust outputs
// are directly byte-comparable.

#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <json-c/json.h>
#include "mypaint-brush.h"
#include "mypaint-surface.h"
#include "mypaint-tiled-surface.h"

#define FIX15_ONE 32768

// ---------------------------------------------------------------------------
// Minimal Surface2-backed tile grid.
//
// Tiles are 64×64 fix15 RGBA `uint16_t[4]`, indexed by `(tx, ty)`. We
// allocate the full grid up front since the script gives us a fixed
// canvas size — keeps lookup branch-free during the dab loop.
// ---------------------------------------------------------------------------

typedef struct {
    MyPaintTiledSurface2 base;
    int tiles_x;
    int tiles_y;
    uint16_t *grid; // tiles_x * tiles_y * (TILE_SIZE * TILE_SIZE * 4) u16
    // Scratch tile handed out for out-of-canvas requests. libmypaint asks
    // for tiles covering the dab's whole bounding box, including beyond
    // the canvas border — returning NULL crashes its dab loop.
    uint16_t *scratch;
} GridSurface;

static inline uint16_t *grid_tile(GridSurface *g, int tx, int ty) {
    if (tx < 0 || ty < 0 || tx >= g->tiles_x || ty >= g->tiles_y) {
        // Wipe the scratch before lending it out so writes from previous
        // OOB requests don't bleed into this one.
        memset(g->scratch, 0,
               (size_t)MYPAINT_TILE_SIZE * MYPAINT_TILE_SIZE * 4 * sizeof(uint16_t));
        return g->scratch;
    }
    const int tile_pixels = MYPAINT_TILE_SIZE * MYPAINT_TILE_SIZE * 4;
    return g->grid + ((size_t)(ty * g->tiles_x + tx) * tile_pixels);
}

static void grid_tile_request_start(MyPaintTiledSurface2 *self_, MyPaintTileRequest *req) {
    GridSurface *self = (GridSurface *)self_;
    req->buffer = grid_tile(self, req->tx, req->ty);
}

static void grid_tile_request_end(MyPaintTiledSurface2 *self_, MyPaintTileRequest *req) {
    (void)self_; (void)req;
}

static MyPaintSurfaceDrawDabFunction2 orig_draw_dab_2;
static int trace_dab_count;

static int trace_draw_dab_2(
    MyPaintSurface2 *self, float x, float y, float radius,
    float r, float g, float b, float opaque, float hardness,
    float alpha_eraser, float aspect_ratio, float angle,
    float lock_alpha, float colorize,
    float posterize, float posterize_num, float paint)
{
    trace_dab_count++;
    fprintf(stderr,
        "  lmp#%d: (%6.2f,%6.2f) r=%5.2f hard=%4.2f opaq=%4.2f aspect=%4.2f ang=%6.1f paint=%4.2f\n",
        trace_dab_count, x, y, radius, hardness, opaque, aspect_ratio, angle, paint);
    return orig_draw_dab_2(self, x, y, radius, r, g, b, opaque, hardness,
                          alpha_eraser, aspect_ratio, angle, lock_alpha, colorize,
                          posterize, posterize_num, paint);
}

static GridSurface *grid_surface_new(int width, int height) {
    GridSurface *g = calloc(1, sizeof(GridSurface));
    g->tiles_x = (width + MYPAINT_TILE_SIZE - 1) / MYPAINT_TILE_SIZE;
    g->tiles_y = (height + MYPAINT_TILE_SIZE - 1) / MYPAINT_TILE_SIZE;
    const size_t tile_pixels = MYPAINT_TILE_SIZE * MYPAINT_TILE_SIZE * 4;
    g->grid = calloc((size_t)g->tiles_x * g->tiles_y * tile_pixels, sizeof(uint16_t));
    g->scratch = calloc(tile_pixels, sizeof(uint16_t));
    mypaint_tiled_surface2_init(&g->base, grid_tile_request_start, grid_tile_request_end);
    return g;
}

static void grid_surface_free(GridSurface *g) {
    mypaint_tiled_surface2_destroy(&g->base);
    free(g->grid);
    free(g->scratch);
    free(g);
}

// ---------------------------------------------------------------------------

static float linear_to_srgb(float v) {
    if (v <= 0.0f) return 0.0f;
    if (v >= 1.0f) return 1.0f;
    if (v <= 0.0031308f) return 12.92f * v;
    return 1.055f * powf(v, 1.0f / 2.4f) - 0.055f;
}

static char *slurp(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) { perror(path); return NULL; }
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    char *buf = malloc(n + 1);
    if (!buf) { fclose(f); return NULL; }
    if (fread(buf, 1, n, f) != (size_t)n) {
        free(buf); fclose(f); return NULL;
    }
    buf[n] = '\0';
    fclose(f);
    return buf;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s <script.json> <brush.myb>\n", argv[0]);
        return 2;
    }

    char *script_text = slurp(argv[1]);
    if (!script_text) return 1;
    char *brush_text = slurp(argv[2]);
    if (!brush_text) return 1;

    struct json_object *script = json_tokener_parse(script_text);
    if (!script) {
        fprintf(stderr, "script JSON parse failed\n");
        return 1;
    }

    struct json_object *jw, *jh, *jevents;
    json_object_object_get_ex(script, "width", &jw);
    json_object_object_get_ex(script, "height", &jh);
    json_object_object_get_ex(script, "events", &jevents);

    int width = json_object_get_int(jw);
    int height = json_object_get_int(jh);
    int n_events = json_object_array_length(jevents);

    MyPaintBrush *brush = mypaint_brush_new();
    if (!mypaint_brush_from_string(brush, brush_text)) {
        fprintf(stderr, "brush parse failed\n");
        return 1;
    }
    mypaint_brush_reset(brush);
    mypaint_brush_new_stroke(brush);

    GridSurface *gs = grid_surface_new(width, height);
    MyPaintSurface2 *surface2 = &gs->base.parent;
    MyPaintSurface *surface = &surface2->parent;

    // Optionally trace every dab to stderr. Set HOKUSAI_TRACE_DABS=1 to
    // enable; the output mirrors hokusai-compat's `debug_dabs` example for
    // direct comparison while diagnosing parity gaps.
    if (getenv("HOKUSAI_TRACE_DABS")) {
        orig_draw_dab_2 = surface2->draw_dab_pigment;
        surface2->draw_dab_pigment = trace_draw_dab_2;
    }

    // Warm-up: libmypaint's first stroke_to applies slow_tracking smoothing
    // BEFORE detecting the reset_requested flag, so for brushes with heavy
    // `slow_tracking` the seeded STATE.X bleeds toward the default 0, and
    // the next event renders dabs along a phantom path from (0,0) to the
    // real start. Trigger libmypaint's "dtime > max_dtime (5s)" branch with
    // a large dt to seed STATE.X cleanly to the first event's position.
    if (n_events > 0) {
        struct json_object *first = json_object_array_get_idx(jevents, 0);
        float sx = (float)json_object_get_double(json_object_array_get_idx(first, 0));
        float sy = (float)json_object_get_double(json_object_array_get_idx(first, 1));
        mypaint_brush_stroke_to_2(brush, surface2, sx, sy, 0.0f, 0.0f, 0.0f, 10.0,
                                  /*viewzoom=*/1.0f, /*viewrotation=*/0.0f, /*barrel_rotation=*/0.0f);
    }

    mypaint_surface_begin_atomic(surface);
    for (int i = 0; i < n_events; i++) {
        struct json_object *ev = json_object_array_get_idx(jevents, i);
        float x  = (float)json_object_get_double(json_object_array_get_idx(ev, 0));
        float y  = (float)json_object_get_double(json_object_array_get_idx(ev, 1));
        float p  = (float)json_object_get_double(json_object_array_get_idx(ev, 2));
        double dt =       json_object_get_double(json_object_array_get_idx(ev, 3));
        mypaint_brush_stroke_to_2(brush, surface2, x, y, p, 0.0f, 0.0f, dt,
                                  1.0f, 0.0f, 0.0f);
    }
    // Call the Surface2 end_atomic directly: the libmypaint
    // `end_atomic_wrapper` shim wraps a NULL `MyPaintRectangle*` into a
    // `MyPaintRectangles{ num_rectangles=1, rectangles=NULL }`, which
    // crashes when end_atomic_2 dereferences `rectangles[0]`.
    mypaint_surface2_end_atomic(surface2, NULL);

    // Flatten tiles to RGBA8 over white, sRGB-encoded.
    uint8_t *out = malloc((size_t)width * height * 4);
    for (size_t i = 0; i < (size_t)width * height; i++) {
        out[i*4+0] = 255;
        out[i*4+1] = 255;
        out[i*4+2] = 255;
        out[i*4+3] = 255;
    }

    const int TS = MYPAINT_TILE_SIZE;
    for (int ty = 0; ty < gs->tiles_y; ty++) {
        for (int tx = 0; tx < gs->tiles_x; tx++) {
            uint16_t *buf = grid_tile(gs, tx, ty);
            if (!buf) continue;
            for (int ly = 0; ly < TS; ly++) {
                for (int lx = 0; lx < TS; lx++) {
                    int wx = tx * TS + lx;
                    int wy = ty * TS + ly;
                    if (wx >= width || wy >= height) continue;
                    uint16_t *px = &buf[(ly * TS + lx) * 4];
                    float a = (float)px[3] / (float)FIX15_ONE;
                    if (a <= 0.0f) continue;
                    float r = (float)px[0] / (float)FIX15_ONE / a;
                    float g = (float)px[1] / (float)FIX15_ONE / a;
                    float b = (float)px[2] / (float)FIX15_ONE / a;
                    float or_ = r * a + 1.0f * (1.0f - a);
                    float og  = g * a + 1.0f * (1.0f - a);
                    float ob  = b * a + 1.0f * (1.0f - a);
                    size_t idx = ((size_t)wy * width + wx) * 4;
                    out[idx+0] = (uint8_t)lroundf(linear_to_srgb(or_) * 255.0f);
                    out[idx+1] = (uint8_t)lroundf(linear_to_srgb(og)  * 255.0f);
                    out[idx+2] = (uint8_t)lroundf(linear_to_srgb(ob)  * 255.0f);
                    out[idx+3] = 255;
                }
            }
        }
    }

    fwrite(out, 1, (size_t)width * height * 4, stdout);

    free(out);
    free(script_text);
    free(brush_text);
    json_object_put(script);
    mypaint_brush_unref(brush);
    grid_surface_free(gs);
    return 0;
}
