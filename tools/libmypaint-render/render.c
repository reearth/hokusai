// libmypaint-render: drive upstream libmypaint with a hokusai-compat script
// and emit a raw RGBA8 buffer (composited over white, sRGB-encoded) on stdout.
//
// Usage:
//   libmypaint-render <script.json> <brush.myb>
// Stdout: width*height*4 bytes of RGBA8. Width/height come from the script.
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
#include "mypaint-fixed-tiled-surface.h"
#include "mypaint-surface.h"
#include "mypaint-tiled-surface.h"

#define FIX15_ONE 32768

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

    MyPaintFixedTiledSurface *fts = mypaint_fixed_tiled_surface_new(width, height);
    MyPaintSurface *surface = mypaint_fixed_tiled_surface_interface(fts);

    mypaint_surface_begin_atomic(surface);
    for (int i = 0; i < n_events; i++) {
        struct json_object *ev = json_object_array_get_idx(jevents, i);
        float x  = (float)json_object_get_double(json_object_array_get_idx(ev, 0));
        float y  = (float)json_object_get_double(json_object_array_get_idx(ev, 1));
        float p  = (float)json_object_get_double(json_object_array_get_idx(ev, 2));
        double dt =       json_object_get_double(json_object_array_get_idx(ev, 3));
        mypaint_brush_stroke_to(brush, surface, x, y, p, 0.0f, 0.0f, dt);
    }
    mypaint_surface_end_atomic(surface, NULL);

    // Flatten tiles to RGBA8 over white, sRGB-encoded.
    uint8_t *out = malloc((size_t)width * height * 4);
    for (size_t i = 0; i < (size_t)width * height; i++) {
        out[i*4+0] = 255;
        out[i*4+1] = 255;
        out[i*4+2] = 255;
        out[i*4+3] = 255;
    }

    const int TS = MYPAINT_TILE_SIZE;
    int tiles_x = (width + TS - 1) / TS;
    int tiles_y = (height + TS - 1) / TS;

    MyPaintTiledSurface *tsurf = (MyPaintTiledSurface *)fts;
    for (int ty = 0; ty < tiles_y; ty++) {
        for (int tx = 0; tx < tiles_x; tx++) {
            MyPaintTileRequest req;
            mypaint_tile_request_init(&req, 0, tx, ty, /*readonly=*/1);
            tsurf->tile_request_start(tsurf, &req);
            uint16_t *buf = req.buffer;
            if (!buf) {
                tsurf->tile_request_end(tsurf, &req);
                continue;
            }
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
            tsurf->tile_request_end(tsurf, &req);
        }
    }

    fwrite(out, 1, (size_t)width * height * 4, stdout);

    free(out);
    free(script_text);
    free(brush_text);
    json_object_put(script);
    mypaint_brush_unref(brush);
    // MyPaintFixedTiledSurface is destroyed via its surface interface.
    mypaint_surface_unref(surface);
    return 0;
}
