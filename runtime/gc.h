/* Native managed heap and mark-sweep collector (Phase 3).
 *
 * Every Simula object, array descriptor, text frame and text object the native
 * runtime allocates goes through `simrt_gc_alloc` instead of `calloc`, so
 * the collector can enumerate the whole heap. Frame roots are *precise*:
 * generated code (and C tests) push a linked list of root frames whose slots
 * hold the pointer-typed MIR locals. Heap tracing of object payloads remains
 * conservative (any word that lands inside a managed block pins it), including
 * FieldAddr interiors resolved through `simrt_gc_block_at`.
 *
 * Reclamation is on by default: a collection runs every 1024 allocations, the
 * same schedule the MIR interpreter uses. `SIM_GC_EVERY=N` overrides it and
 * `SIM_GC_EVERY=0` turns automatic collection off, leaving the managed heap
 * and the explicit `simrt_gc_collect()` entry point in place. Swept blocks
 * go on an exact-size free list rather than back to the host allocator, so a
 * loop that churns one shape of object keeps landing in the slot it just
 * vacated.
 */

#ifndef SIMRT_GC_H
#define SIMRT_GC_H

#include <stddef.h>
#include <stdint.h>

#include "annot.h"

/* What a block contains, which decides how it is traced. */
typedef enum {
    /* Class instance: `class_id` then attribute words. Traced conservatively. */
    SIMRT_GC_OBJECT = 0,
    /* `{ ndims, bounds[2*ndims], data[] }` with int64/ref payload. Ref arrays
     * lower to this kind too, so it is traced conservatively. */
    SIMRT_GC_ARRAY_I64 = 1,
    /* Same descriptor, `double` payload: never holds a pointer. */
    SIMRT_GC_ARRAY_F64 = 2,
    /* Same descriptor, `SimrtTextFrame *` payload: traced precisely. */
    SIMRT_GC_ARRAY_TEXT = 3,
    /* `SimrtTextFrame`, whose first word is its text object. */
    SIMRT_GC_TEXT_FRAME = 4,
    /* `SimrtTextObject` header followed by its character bytes in the same
     * managed block (Phase 3 step 2). Interior content pointers pin it. */
    SIMRT_GC_TEXT_OBJECT = 5,
    /* Runtime-internal record with no traceable Simula fields (SYSIN/SYSOUT). */
    SIMRT_GC_OPAQUE = 6
} simrt_gc_kind;

/* Reached by the current trace. Cleared at the start of every collection. */
#define SIMRT_GC_FLAG_MARK 0x1u
/* Never collected, and a root for this collection (hook for blocks handed to
 * foreign code). */
#define SIMRT_GC_FLAG_PINNED 0x2u
/* Traced and rooted, but never swept. Retained for rare foreign/pinned cases;
 * texts no longer use this after Phase 3 step 2. */
#define SIMRT_GC_FLAG_UNSWEPT 0x4u

/* `size` MUST stay the last member: the object ABI reads the payload size from
 * `obj - 8` (`simrt_object_load_i64` bounds checks), so the eight bytes
 * immediately before a payload have to be that size. */
typedef struct SimrtGcHeader {
    struct SimrtGcHeader *next;
    uint32_t kind;
    uint32_t flags;
#if defined(SIMRT_GC_HEADER_NEEDS_PAD)
    uint32_t pad[SIMRT_GC_HEADER_NEEDS_PAD];
#elif SIZE_MAX <= 0xffffffffu
    uint32_t pad[1];
#endif
    int64_t size;
} SimrtGcHeader;

/* Linked list of precise root frames. Cranelift emits one per generated
 * function that has GC-typed locals; C tests push them by hand. Layout is this
 * 16-byte header followed by `nslots` pointer-sized values:
 *   [prev:8][nslots:u32][pad:u32][slot0][slot1]…
 * Slots hold pointer values (including FieldAddr interiors), not `void **`. */
typedef struct SimrtGcRootFrame {
    struct SimrtGcRootFrame *prev;
    uint32_t nslots;
    uint32_t pad;
} SimrtGcRootFrame;

#define SIMRT_GC_ROOT_HEADER_SIZE 16u

/* Allocates `size` zeroed payload bytes. Returns the payload pointer, or NULL
 * on genuine host OOM so each call site keeps its own diagnostic. May collect
 * first when a threshold is configured. */
/* gc-roots: none — caller must store the payload before the next safepoint. */
SIMRT_MUST_USE simrt_gc_ptr simrt_gc_alloc(simrt_gc_kind kind, int64_t size);

/* As above with initial header flags (`SIMRT_GC_FLAG_UNSWEPT` for texts). */
SIMRT_MUST_USE simrt_gc_ptr simrt_gc_alloc_flagged(
    simrt_gc_kind kind, int64_t size, uint32_t flags
);

/* Runs one full mark-sweep. A no-op while a collection is already running, or
 * once the collector has been disabled. Exported for tests. */
void simrt_gc_collect(void);

/* Stops all future collection. Called on the `simrt_error` path so a
 * failing program never trips over a half-walked heap. */
void simrt_gc_disable(void);

/* Non-zero when a collection could actually run here. */
int simrt_gc_enabled(void);

int64_t simrt_gc_live_blocks(void);
int64_t simrt_gc_live_bytes(void);
int64_t simrt_gc_collections(void);
int64_t simrt_gc_blocks_freed(void);
int64_t simrt_gc_bytes_freed(void);
int64_t simrt_gc_allocations(void);
/* Allocations served from the free list instead of the host allocator. */
int64_t simrt_gc_slots_reused(void);
/* Nanoseconds spent inside `simrt_gc_collect`, summed over the run. */
int64_t simrt_gc_pause_ns(void);

/* Link `frame` as the current function's precise roots. `nslots` is the number
 * of pointer slots that follow the 16-byte header; they are zeroed. */
void simrt_gc_root_push(void *frame, int64_t nslots);
/* Unlink `frame`. Must be the current head (LIFO). */
void simrt_gc_root_pop(void *frame);
/* Head of the running coroutine's root-frame list. Used to save/restore across
 * stack switches. */
void *simrt_gc_root_head(void);
void simrt_gc_root_set_head(void *frame);

/* Skip automatic collection while a runtime helper holds an in-progress
 * pointer in a C local. Nestable. Prefer `SIMRT_GC_DEFER_BEGIN` /
 * `SIMRT_GC_DEFER_LEAVE` so an early return still pairs them. `abort` /
 * `exit` skip GNU cleanup (the process is dying). */
void simrt_gc_defer_collect(void);
void simrt_gc_allow_collect(void);

typedef struct simrt_gc_defer_guard {
    int active;
} simrt_gc_defer_guard;

static inline void simrt_gc_defer_guard_enter(simrt_gc_defer_guard *guard) {
    guard->active = 1;
    simrt_gc_defer_collect();
}

static inline void simrt_gc_defer_guard_leave(simrt_gc_defer_guard *guard) {
    if (guard != NULL && guard->active) {
        guard->active = 0;
        simrt_gc_allow_collect();
    }
}

#define SIMRT_GC_DEFER_BEGIN()                                                               \
    simrt_gc_defer_guard _simrt_gc_defer SIMRT_CLEANUP(simrt_gc_defer_guard_leave) = {0}; \
    simrt_gc_defer_guard_enter(&_simrt_gc_defer)

#define SIMRT_GC_DEFER_LEAVE() simrt_gc_defer_guard_leave(&_simrt_gc_defer)

/* Root callback handed to the enumerators below. Safe to call with NULL, with
 * a non-heap address, or with an interior pointer into a managed block. */
typedef void (*simrt_gc_mark_fn)(simrt_gc_ptr pointer);

/* C runtime globals: SYSIN / SYSOUT, open BASICIO file objects, and the SQS
 * (`runtime/io.c`, `runtime/sim.c`, dispatched from `runtime/runtime.c`). */
void simrt_gc_visit_runtime_roots(simrt_gc_mark_fn mark);

/* Every registered quasi-parallel component's object, which is how a detached
 * object reachable only through its reactivation chain stays live
 * (`runtime/sequencing.c`). */
void simrt_seq_gc_visit_roots(simrt_gc_mark_fn mark);

/* Host-held object handles. Id 0 is `none`.
 * The table lives at the embedding boundary and is walked by the collector. */
int64_t simrt_ref_pin(simrt_gc_ptr ref);
void simrt_ref_unpin(int64_t id);
simrt_gc_ptr simrt_ref_get(int64_t id);

#endif /* SIMRT_GC_H */
