//! Request-scoped bump allocator occupying the imported linear-memory slab.
//!
//! Generated Simula code bump-allocates from `HEAP_CURSOR` above `TEXT_BASE`.
//! This module's `Vec`/`String` traffic stays inside [`super::layout::C_RT_END`]
//! so the two heaps cannot meet. Each exported helper snapshots the bump
//! pointer and restores it on return, so dropped `Vec`s do not need a free list.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::ptr;

use crate::layout::C_RT_END;

#[cfg(target_arch = "wasm32")]
unsafe extern "C" {
    static __heap_base: u8;
}

thread_local! {
    static POS: Cell<u32> = const { Cell::new(0) };
}

pub struct Arena;

fn heap_base() -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        &raw const __heap_base as u32
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

fn current() -> u32 {
    POS.with(|pos| {
        let mut value = pos.get();
        if value == 0 {
            value = heap_base();
            pos.set(value);
        }
        value
    })
}

/// Run `f` and rewind the bump pointer so the next helper starts clean.
pub fn with_arena<T>(f: impl FnOnce() -> T) -> T {
    let saved = current();
    let result = f();
    POS.with(|pos| pos.set(saved));
    result
}

unsafe impl GlobalAlloc for Arena {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        POS.with(|pos| {
            let mut cursor = pos.get();
            if cursor == 0 {
                cursor = heap_base();
            }
            let align = layout.align().max(1) as u32;
            cursor = cursor.saturating_add(align - 1) & !(align - 1);
            let size = layout.size() as u32;
            let end = match cursor.checked_add(size) {
                Some(end) if end <= C_RT_END => end,
                _ => return ptr::null_mut(),
            };
            pos.set(end);
            cursor as *mut u8
        })
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOC: Arena = Arena;
