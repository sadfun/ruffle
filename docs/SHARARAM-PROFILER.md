# Shararam profiler build

The `shararam_profiler` cargo feature (crates `ruffle_core` and `ruffle_web`)
turns Ruffle into a self-profiling build for the Shararam Ruffle client.
It is **off by default**; regular builds compile every hook in
`core/src/profiler.rs` to nothing.

What it records (all timestamped on `performance.now()`):

| category   | events                                                                 |
|------------|------------------------------------------------------------------------|
| `frame`    | `host_tick` (rAF handler), `tick`, `run_frame`                          |
| `script`   | `avm1_frame`, `run_actions`, `timers`, `goto`, `rtmp_responder`, `rtmp_invoke`, `load_vars_on_data` |
| `render`   | `render` (with per-frame counters), `submit`, `submit_frame`, `register_shape` (tessellation), `register_bitmap`, `update_texture`, `render_offscreen`, `apply_filter`, `gpu_readback`, `viewport` |
| `load`     | `movie_load_start`, `movie_data`, `preload`, `preload_tick`, `movie_complete`, `movie_init_queued`, `movie_error`, `root_movie` |
| `swf`      | `preload_chunk` (tag parsing, per movie/sprite), `define_font`           |
| `asset`    | `bitmap_decode`                                                         |
| `http`     | `fetch` (every `Player::fetch`, with status/bytes/headers time)          |
| `rtmp`     | `connect`, `socket_open`, `transport`, `handshake_complete`, `send`, `recv` (AMF arguments rendered as JSON), `data_in` |
| `net`      | `update_sockets`, `update_net_connections`, `streams`                   |
| `gc`       | `collect`                                                               |
| `input`    | `handle_event`, `mouse_pick`                                            |
| `text`     | `relayout`                                                              |
| `external` | `call_out` / `call_in` (ExternalInterface)                              |
| `sampler`  | `avm1` — weighted AVM1 stack samples (see below)                        |

Short, high-frequency regions carry a minimum duration (see the call sites),
so idle frames do not flood the buffer.

## AVM1 stack sampler

The interpreter loop ticks the profiler once per executed action; about once
per millisecond of AVM1 execution the current activation chain is rendered to
a string (`"[Frame] / onEnterFrame / moveChar"`) and recorded as a span
covering the time since the previous sample. Entering AVM1 from outside
(a fresh root activation) resets the window, and a root activation flushes
its tail on drop, so the sum of sample durations approximates total AVM1
execution time; native calls are folded into their bytecode caller. Real
function names come from the same lookup `avm_debug` uses, but without the
argument formatting (see `Avm1Function::exec`).

## Allocation counters and screen grid

`Object::new_impl`, `ArrayBuilder::init_with` and `FunctionObject::build`
count AVM1 object creations (`avm1_objects` / `avm1_arrays` /
`avm1_functions` in the per-frame `render` event args, next to
`objects_instantiated` etc.); each stack sample also carries the number of
objects created inside its window (`alloc`), which gives ~1 ms allocation
backtraces. The `render` event additionally reports `gc_bytes` — the
gc-arena heap size. AvmString allocations are **not** counted (the string
type lives in `ruffle_common`, outside this crate).

`ProfiledRenderer::submit_frame` walks the frame's command list and emits a
`render/screen_grid` instant per frame: a 24×18 grid of how many draw
commands cover each viewport cell (plus a separate layer for commands inside
blend/alpha-mask subtrees, and the screen rects of those subtrees). Shape
bounds and bitmap sizes are remembered at registration; command matrices are
absolute, so the AABBs are directly in viewport pixels.

The events are exposed to the page as `window.__ruffleProfiler.drain()`
(a JSON array string; see `web/src/profiler.rs`). The Shararam client polls
it and stores everything in a DuckDB file.

Build the self-hosted bundle with the feature enabled (same release profile
and `wasm-opt` settings as a normal build, so performance stays
representative):

```sh
cd web
npm install
npm run build:shararam-profiler
# -> web/packages/selfhosted/dist/{ruffle.js, core.ruffle.*.js, *.wasm}
```
