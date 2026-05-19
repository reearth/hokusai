# Hokusai WASM demo

A small browser canvas that uses `hokusai-wasm` to draw with real
libmypaint brushes.

Hosted version: <https://reearth.github.io/hokusai/> (deployed by
`.github/workflows/pages.yml` on every push to `main`).

The rest of this file is the recipe for running it locally.

## Build

```sh
# from the repo root
wasm-pack build crates/hokusai-wasm --target web --out-dir demo/pkg

# stage the .myb fixtures the demo loads
mkdir -p crates/hokusai-wasm/demo/brushes
cp hokusai/examples/fixtures/*.myb crates/hokusai-wasm/demo/brushes/
```

## Serve

`fetch()` of the `.myb` and the wasm module requires HTTP (not `file://`):

```sh
cd crates/hokusai-wasm/demo
python3 -m http.server 8000
# open http://localhost:8000/
```

## Controls

- Brush dropdown — switches between vendored libmypaint brushes
- Colour picker — base HSV colour
- Size slider — `radius_logarithmic` base value (log₂ pixels)
- Clear — resets the canvas
- Drag to paint. Pen pressure works on devices that expose it via
  PointerEvent.pressure; mice fall back to a fixed 0.5.
