//! Global allocator that grows wasm memory in 64 MiB chunks.
//!
//! std's dlmalloc grows the wasm heap 64 KiB at a time, one `memory.grow`
//! per chunk. On Windows/Chromium every grow costs 1–10 ms in the kernel
//! (`BackingStore::GrowWasmMemoryInPlace` → `VirtualAlloc`), so a single
//! tick that needs 30 MB of fresh heap made ~500 grows and froze the game
//! for seconds. Same dlmalloc, bigger sbrk.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use dlmalloc::{Allocator, Dlmalloc};

const PAGE: usize = 64 * 1024;
const CHUNK: usize = 64 * 1024 * 1024;

struct Chunky;

unsafe impl Allocator for Chunky {
    fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
        // dlmalloc asks for multiples of PAGE; CHUNK is one too.
        let mut pages = size.max(CHUNK) / PAGE;
        let mut prev = core::arch::wasm32::memory_grow(0, pages);
        if prev == usize::MAX && size < CHUNK {
            // Near the memory limit: settle for what was asked.
            pages = size / PAGE;
            prev = core::arch::wasm32::memory_grow(0, pages);
        }
        if prev == usize::MAX {
            return (core::ptr::null_mut(), 0, 0);
        }
        ((prev * PAGE) as *mut u8, pages * PAGE, 0)
    }

    fn remap(&self, _ptr: *mut u8, _oldsize: usize, _newsize: usize, _can_move: bool) -> *mut u8 {
        core::ptr::null_mut()
    }

    fn free_part(&self, _ptr: *mut u8, _oldsize: usize, _newsize: usize) -> bool {
        false
    }

    fn free(&self, _ptr: *mut u8, _size: usize) -> bool {
        false
    }

    fn can_release_part(&self, _flags: u32) -> bool {
        false
    }

    fn allocates_zeros(&self) -> bool {
        true
    }

    fn page_size(&self) -> usize {
        PAGE
    }
}

struct ChunkyDlmalloc(UnsafeCell<Dlmalloc<Chunky>>);

// wasm32-unknown-unknown without atomics is single-threaded, same as
// dlmalloc's own `GlobalDlmalloc` assumes.
unsafe impl Sync for ChunkyDlmalloc {}

unsafe impl GlobalAlloc for ChunkyDlmalloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { (*self.0.get()).malloc(layout.size(), layout.align()) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { (*self.0.get()).free(ptr, layout.size(), layout.align()) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe { (*self.0.get()).calloc(layout.size(), layout.align()) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe { (*self.0.get()).realloc(ptr, layout.size(), layout.align(), new_size) }
    }
}

#[global_allocator]
static ALLOC: ChunkyDlmalloc =
    ChunkyDlmalloc(UnsafeCell::new(Dlmalloc::new_with_allocator(Chunky)));
