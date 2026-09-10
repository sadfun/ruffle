//! Mouse-pick micro-benchmark on a synthetic crowded scene.
//!
//!     python3 core/examples/pickbench_gen_crowd.py crowd.swf 25 12 2 2 2 1 3
//!     cargo run --release -p ruffle_core --features pick_stats --example pickbench -- crowd.swf [iters]
//!
//! Builds a headless player (null renderer), runs a few frames so the timeline
//! and `DoAction`s instantiate the tree, then times `run_mouse_pick` and the
//! full `MouseMove` event at several cursor positions.

use ruffle_core::tag_utils::SwfMovie;
use ruffle_core::{PlayerBuilder, PlayerEvent, pick_stats};
use std::time::Instant;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: pickbench <swf> [iters]");
    let iters: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let data = std::fs::read(&path).expect("read swf");
    let movie =
        SwfMovie::from_data(&data, "file:///bench.swf".to_string(), None, None).expect("parse swf");
    let player = PlayerBuilder::new()
        .with_movie(movie)
        .with_viewport_dimensions(815, 495, 1.0)
        .with_autoplay(true)
        .build();
    let mut p = player.lock().unwrap();
    p.set_mouse_in_stage(true);
    for _ in 0..3 {
        p.run_frame();
    }

    // (label, x, y) in stage pixels: over the crowd centre, over the empty
    // floor (click zone only), over a UI button, over nothing (top-left corner
    // above the floor? the floor covers everything, so use the ui strip gap).
    let positions = [
        ("crowd", 407.0, 297.0),
        ("crowd2", 350.0, 250.0),
        ("floor", 60.0, 470.0),
        ("ui", 30.0, 30.0),
    ];
    println!("swf={path} iters={iters}");
    let t = Instant::now();
    for _ in 0..50 {
        p.run_frame();
    }
    println!(
        "run_frame {:.1} us (static scene: enterFrame dispatch only)",
        t.elapsed().as_secs_f64() * 1e6 / 50.0
    );
    for (label, x, y) in positions {
        for _ in 0..5 {
            p.run_frame();
        }
        p.handle_event(PlayerEvent::MouseMove { x, y });
        for _ in 0..10 {
            p.bench_mouse_pick();
        }
        pick_stats::take();
        pick_stats::take_timers();
        let t = Instant::now();
        let mut target = None;
        for _ in 0..iters {
            target = p.bench_mouse_pick();
        }
        let pick_us = t.elapsed().as_secs_f64() * 1e6 / iters as f64;
        let stats = pick_stats::take();
        let timers = pick_stats::take_timers();
        let t = Instant::now();
        for i in 0..iters {
            p.handle_event(PlayerEvent::MouseMove {
                x: x + (i % 2) as f64,
                y,
            });
        }
        let move_us = t.elapsed().as_secs_f64() * 1e6 / iters as f64;
        let counters: Vec<String> = pick_stats::NAMES
            .iter()
            .zip(stats.iter())
            .filter(|(n, _)| !n.is_empty())
            .map(|(n, v)| format!("{n}={:.0}", *v as f64 / iters as f64))
            .collect();
        println!(
            "{label:7} pick {pick_us:8.1} us   mouse_move {move_us:8.1} us   target={}",
            target.as_deref().unwrap_or("-")
        );
        let timers: Vec<String> = pick_stats::TIMER_NAMES
            .iter()
            .zip(timers.iter())
            .filter(|(n, _)| !n.is_empty())
            .map(|(n, v)| {
                format!(
                    "{}={:.1}us",
                    n.trim_end_matches("_ns"),
                    *v as f64 / 1e3 / iters as f64
                )
            })
            .collect();
        println!(
            "        per pick: {}  {}",
            counters.join(" "),
            timers.join(" ")
        );
    }
}
