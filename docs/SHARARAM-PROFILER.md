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

Short, high-frequency regions carry a minimum duration (see the call sites),
so idle frames do not flood the buffer.

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
