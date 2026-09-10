//! Counters for the mouse-pick benchmark (`cargo run --example pickbench`).
//! Compiled to nothing without the `pick_stats` feature.

#[cfg(feature = "pick_stats")]
mod imp {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    pub const NAMES: [&str; 8] = [
        "pick_nodes",
        "l2g_calls",
        "l2g_steps",
        "bounds_nodes",
        "button_mode_slow",
        "hit_test_shape",
        "shape_tests",
        "inverse",
    ];
    static COUNTERS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

    #[inline]
    pub fn bump(i: usize) {
        COUNTERS[i].fetch_add(1, Relaxed);
    }

    pub fn take() -> [u64; 8] {
        std::array::from_fn(|i| COUNTERS[i].swap(0, Relaxed))
    }

    pub const TIMER_NAMES: [&str; 3] = ["button_mode_ns", "hit_test_shape_ns", "masker_ns"];
    static TIMERS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

    pub struct Timer(usize, std::time::Instant);
    impl Drop for Timer {
        fn drop(&mut self) {
            TIMERS[self.0].fetch_add(self.1.elapsed().as_nanos() as u64, Relaxed);
        }
    }
    pub fn timer(i: usize) -> Timer {
        Timer(i, std::time::Instant::now())
    }
    pub fn take_timers() -> [u64; 3] {
        std::array::from_fn(|i| TIMERS[i].swap(0, Relaxed))
    }
}

#[cfg(not(feature = "pick_stats"))]
mod imp {
    pub const NAMES: [&str; 8] = [""; 8];
    #[inline(always)]
    pub fn bump(_i: usize) {}
    pub fn take() -> [u64; 8] {
        [0; 8]
    }
    pub const TIMER_NAMES: [&str; 3] = [""; 3];
    pub struct Timer;
    #[inline(always)]
    pub fn timer(_i: usize) -> Timer {
        Timer
    }
    pub fn take_timers() -> [u64; 3] {
        [0; 3]
    }
}

pub use imp::{NAMES, TIMER_NAMES, Timer, bump, take, take_timers, timer};

pub const PICK_NODES: usize = 0;
pub const L2G_CALLS: usize = 1;
pub const L2G_STEPS: usize = 2;
pub const BOUNDS_NODES: usize = 3;
pub const BUTTON_MODE_SLOW: usize = 4;
pub const HIT_TEST_SHAPE: usize = 5;
pub const SHAPE_TESTS: usize = 6;
pub const INVERSE: usize = 7;
pub const T_BUTTON_MODE: usize = 0;
pub const T_HIT_TEST_SHAPE: usize = 1;
pub const T_MASKER: usize = 2;
