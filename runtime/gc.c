/* Native managed heap and mark-sweep collector. See gc.h for the contract.
 *
 * Frame references are precise: Cranelift emits a linked list of root frames
 * whose slots hold every GC-typed MIR local (ObjectRef, Text, Array*, RefI64).
 * A parked coroutine saves the head of that list on switch, so the collector
 * walks the running chain plus every other component's saved head. Heap object
 * payloads are still scanned conservatively (any word inside a managed block
 * pins it), and FieldAddr interiors in those slots hit via `block_at`.
 *
 * Nested `gc_alloc` inside a runtime helper does not collect while
 * `simrt_gc_defer_collect` is held (text_from_literal, array_alloc_text).
 * Generated callers already have their frames on the root list.
 */

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "gc.h"
#include "host.h"

#include "coro.h"

#if defined(_WIN32)
#include <windows.h>
#endif

/* The object ABI depends on this: `simrt_object_size` reads `obj - 8`. */
typedef char simrt_gc_header_layout_check[(sizeof(SimrtGcHeader) % 8u == 0u
                                             && offsetof(SimrtGcHeader, size) + sizeof(int64_t)
                                                    == sizeof(SimrtGcHeader))
                                                ? 1
                                                : -1];

typedef char simrt_gc_root_frame_layout_check[(sizeof(SimrtGcRootFrame) == SIMRT_GC_ROOT_HEADER_SIZE)
                                                    ? 1
                                                    : -1];

#define SIMRT_GC_WORD sizeof(void *)

/* How much of a reclaimed block to overwrite. Covers the leading fields
 * (class_id, SIMSET links, the first attributes) without paying a full memset
 * for a large array. */
#define SIM_GC_POISON_BYTES 128u

/* Reuse is exact-size: a request only ever takes a block whose payload is the
 * same width, so nothing has to be split or coalesced. Two bounds keep the
 * single list from degrading. The search gives up after this many candidates —
 * allocation churn of one size finds its block at the head — and the sweep
 * stops hoarding past the cap below, handing the rest back to the host, so a
 * program whose sizes never repeat cannot accumulate a free list it will never
 * draw from. */
#define SIMRT_GC_FREE_SCAN 64u
#define SIMRT_GC_FREE_MAX 4096u

typedef struct {
    /* Every managed block, newest first; the sweep unlinks from here. */
    SimrtGcHeader *blocks;
    size_t block_count;
    size_t live_bytes;

    /* Swept blocks held for reuse, newest first, linked through the same
     * `next`. Deliberately *not* on `blocks`: the mark phase must not see them
     * and the index must not resolve a stale word onto one. */
    SimrtGcHeader *free_list;
    size_t free_count;

    /* Payload address range, for rejecting non-heap words without a search. */
    uintptr_t heap_lo;
    uintptr_t heap_hi;

    /* Blocks sorted by payload address, rebuilt per collection so a candidate
     * word (possibly interior) can be resolved by binary search. */
    SimrtGcHeader **index;
    size_t index_len;
    size_t index_cap;

    /* Marked-but-not-yet-traced blocks. */
    SimrtGcHeader **gray;
    size_t gray_len;
    size_t gray_cap;
    int gray_overflow;

    /* Allocations between automatic collections; 0 disables them entirely. */
    uint64_t threshold;
    uint64_t since_collect;

    int inited;
    int disabled;
    int collecting;
    int stats;

    /* Nesting of `simrt_gc_defer_collect`. Automatic collection is
     * skipped while a runtime helper holds an in-progress pointer in a C
     * local (text_from_literal, array_alloc_text, …). */
    int defer_collect;

    /* Precise root-frame list of the running coroutine. */
    SimrtGcRootFrame *roots;

    /* When set, the sweep poisons the entire payload instead of the leading
     * `SIM_GC_POISON_BYTES`. `SIM_GC_POISON=1` (or any non-empty
     * non-"0" value) enables it. */
    int poison_all;

    uint64_t collections;
    uint64_t blocks_freed;
    uint64_t bytes_freed;
    uint64_t allocations;
    uint64_t slots_reused;
    uint64_t pause_ns;
    size_t peak_blocks;
    size_t peak_bytes;
} SimrtGcState;

static SimrtGcState g_gc;

/* Embedder pin table. Slot 0 is `none`. */
#define SIMRT_REF_PIN_MAX 256
static simrt_gc_ptr g_ref_pins[SIMRT_REF_PIN_MAX];
static void simrt_ref_gc_visit_roots(simrt_gc_mark_fn mark);

static void *simrt_gc_payload(SimrtGcHeader *header) {
    return (void *)(header + 1);
}

/* ------------------------------------------------------------------ */
/* Initialization                                                      */
/* ------------------------------------------------------------------ */

static void simrt_gc_report(void);

static void simrt_gc_init(void) {
    const char *every;
    const char *stats;
    const char *poison;

    if (g_gc.inited) {
        return;
    }
    g_gc.inited = 1;

    /* Same schedule as the MIR interpreter's DEFAULT_GC_THRESHOLD, so a
     * program reclaims on both backends at the same points. */
    g_gc.threshold = 1024u;

    every = getenv("SIM_GC_EVERY");
    if (every != NULL && *every != '\0') {
        char *end = NULL;
        unsigned long long parsed = strtoull(every, &end, 10);
        if (end != NULL && *end == '\0') {
            /* `0` is a meaningful setting: no automatic collection at all. */
            g_gc.threshold = (uint64_t)parsed;
        }
    }

    stats = getenv("SIM_GC_STATS");
    g_gc.stats = stats != NULL && stats[0] != '\0' && stats[0] != '0';
    if (g_gc.stats) {
        atexit(simrt_gc_report);
    }

    poison = getenv("SIM_GC_POISON");
    g_gc.poison_all = poison != NULL && poison[0] != '\0' && poison[0] != '0';
}

/* Stats go to stderr only: stdout is the program's Simula output. */
static void simrt_gc_report(void) {
    fprintf(
        stderr,
        "sim gc: collections=%llu blocks_freed=%llu bytes_freed=%llu "
        "live_blocks=%llu live_bytes=%llu peak_blocks=%llu peak_bytes=%llu "
        "allocations=%llu slots_reused=%llu pause_ns=%llu threshold=%llu enabled=%d\n",
        (unsigned long long)g_gc.collections,
        (unsigned long long)g_gc.blocks_freed,
        (unsigned long long)g_gc.bytes_freed,
        (unsigned long long)g_gc.block_count,
        (unsigned long long)g_gc.live_bytes,
        (unsigned long long)g_gc.peak_blocks,
        (unsigned long long)g_gc.peak_bytes,
        (unsigned long long)g_gc.allocations,
        (unsigned long long)g_gc.slots_reused,
        (unsigned long long)g_gc.pause_ns,
        (unsigned long long)g_gc.threshold,
        g_gc.disabled ? 0 : 1
    );
}

/* ------------------------------------------------------------------ */
/* Allocation                                                          */
/* ------------------------------------------------------------------ */

void *simrt_gc_alloc(simrt_gc_kind kind, int64_t size) {
    return simrt_gc_alloc_flagged(kind, size, 0u);
}

/* Unlinks a swept block whose payload is exactly `size` bytes, or NULL. */
static SimrtGcHeader *simrt_gc_take_free(int64_t size) {
    SimrtGcHeader **link = &g_gc.free_list;
    SimrtGcHeader *block = g_gc.free_list;
    unsigned scanned = 0u;

    while (block != NULL && scanned < SIMRT_GC_FREE_SCAN) {
        if (block->size == size) {
            *link = block->next;
            g_gc.free_count--;
            return block;
        }
        link = &block->next;
        block = block->next;
        scanned++;
    }
    return NULL;
}

static void simrt_gc_maybe_auto_collect(void) {
    if (g_gc.threshold == 0 || g_gc.disabled || g_gc.collecting || g_gc.defer_collect != 0) {
        return;
    }
    g_gc.since_collect++;
    if (g_gc.since_collect >= g_gc.threshold) {
        simrt_gc_collect();
    }
}

void *simrt_gc_alloc_flagged(simrt_gc_kind kind, int64_t size, uint32_t flags) {
    SimrtGcHeader *header;
    uintptr_t low;
    uintptr_t high;

    if (!g_gc.inited) {
        simrt_gc_init();
    }
    if (size < 0) {
        return NULL;
    }
    if ((uint64_t)size > (uint64_t)(SIZE_MAX - sizeof(SimrtGcHeader))) {
        return NULL;
    }

    /* Collect *before* taking the new block, so the tracer never sees a block
     * whose caller has not had a chance to store a reference to it yet.
     * Deferred helpers skip this: they already collected on entering the
     * deferred region, when no in-progress C local existed yet. */
    simrt_gc_maybe_auto_collect();

    header = simrt_gc_take_free(size);
    if (header != NULL) {
        /* Callers rely on `calloc` semantics, and the sweep left poison here. */
        memset(simrt_gc_payload(header), 0, (size_t)size);
        g_gc.slots_reused++;
    } else {
        header = (SimrtGcHeader *)simrt_host_calloc(1, sizeof(SimrtGcHeader) + (size_t)size);
        if (header == NULL) {
            return NULL;
        }
    }
    header->kind = (uint32_t)kind;
    header->flags = flags;
    header->size = size;
    header->next = g_gc.blocks;
    g_gc.blocks = header;
    g_gc.block_count++;
    g_gc.live_bytes += (size_t)size;
    g_gc.allocations++;

    low = (uintptr_t)simrt_gc_payload(header);
    high = low + (size > 0 ? (uintptr_t)size : 1u);
    if (g_gc.heap_hi == 0 || low < g_gc.heap_lo) {
        g_gc.heap_lo = low;
    }
    if (high > g_gc.heap_hi) {
        g_gc.heap_hi = high;
    }

    if (g_gc.block_count > g_gc.peak_blocks) {
        g_gc.peak_blocks = g_gc.block_count;
    }
    if (g_gc.live_bytes > g_gc.peak_bytes) {
        g_gc.peak_bytes = g_gc.live_bytes;
    }
    return simrt_gc_payload(header);
}

/* ------------------------------------------------------------------ */
/* Block lookup                                                        */
/* ------------------------------------------------------------------ */

static int simrt_gc_index_compare(const void *left, const void *right) {
    const SimrtGcHeader *a = *(const SimrtGcHeader *const *)left;
    const SimrtGcHeader *b = *(const SimrtGcHeader *const *)right;
    if (a < b) {
        return -1;
    }
    return a > b ? 1 : 0;
}

static int simrt_gc_index_build(void) {
    SimrtGcHeader *block;
    if (g_gc.block_count > g_gc.index_cap) {
        size_t cap = g_gc.index_cap == 0 ? 64u : g_gc.index_cap;
        SimrtGcHeader **grown;
        while (cap < g_gc.block_count) {
            if (cap > SIZE_MAX / 2u) {
                return 0;
            }
            cap *= 2u;
        }
        grown = (SimrtGcHeader **)simrt_host_realloc_n(g_gc.index, cap, sizeof(*grown));
        if (grown == NULL) {
            return 0;
        }
        g_gc.index = grown;
        g_gc.index_cap = cap;
    }
    g_gc.index_len = 0;
    for (block = g_gc.blocks; block != NULL; block = block->next) {
        g_gc.index[g_gc.index_len++] = block;
    }
    /* Header addresses sort the same way payload addresses do. */
    qsort(g_gc.index, g_gc.index_len, sizeof(*g_gc.index), simrt_gc_index_compare);
    return 1;
}

/* The block whose payload contains `address`, or NULL. Interior pointers hit. */
static SimrtGcHeader *simrt_gc_block_at(uintptr_t address) {
    size_t low = 0;
    size_t high = g_gc.index_len;
    SimrtGcHeader *candidate;
    uintptr_t start;
    uintptr_t end;

    if (address < g_gc.heap_lo || address >= g_gc.heap_hi) {
        return NULL;
    }
    while (low < high) {
        size_t mid = low + (high - low) / 2u;
        if ((uintptr_t)simrt_gc_payload(g_gc.index[mid]) <= address) {
            low = mid + 1u;
        } else {
            high = mid;
        }
    }
    if (low == 0) {
        return NULL;
    }
    candidate = g_gc.index[low - 1u];
    start = (uintptr_t)simrt_gc_payload(candidate);
    end = start + (candidate->size > 0 ? (uintptr_t)candidate->size : 1u);
    return address < end ? candidate : NULL;
}

/* ------------------------------------------------------------------ */
/* Mark                                                                */
/* ------------------------------------------------------------------ */

static void simrt_gc_push_gray(SimrtGcHeader *block) {
    if (g_gc.gray_len == g_gc.gray_cap) {
        size_t cap;
        SimrtGcHeader **grown;
        if (g_gc.gray_cap > SIZE_MAX / 2u) {
            g_gc.gray_overflow = 1;
            return;
        }
        cap = g_gc.gray_cap == 0 ? 64u : g_gc.gray_cap * 2u;
        grown = (SimrtGcHeader **)simrt_host_realloc_n(g_gc.gray, cap, sizeof(*grown));
        if (grown == NULL) {
            /* Out of scratch memory mid-trace: the block stays marked and the
             * drain loop re-traces marked blocks until nothing new appears. */
            g_gc.gray_overflow = 1;
            return;
        }
        g_gc.gray = grown;
        g_gc.gray_cap = cap;
    }
    g_gc.gray[g_gc.gray_len++] = block;
}

static void simrt_gc_mark(void *pointer) {
    SimrtGcHeader *block;
    if (pointer == NULL) {
        return;
    }
    block = simrt_gc_block_at((uintptr_t)pointer);
    if (block == NULL || (block->flags & SIMRT_GC_FLAG_MARK) != 0) {
        return;
    }
    block->flags |= SIMRT_GC_FLAG_MARK;
    simrt_gc_push_gray(block);
}

static void simrt_gc_scan_words(const void *low, const void *high) {
    uintptr_t start = (uintptr_t)low;
    uintptr_t end = (uintptr_t)high;
    uintptr_t cursor;

    if (start >= end) {
        return;
    }
    start = (start + (SIMRT_GC_WORD - 1u)) & ~(uintptr_t)(SIMRT_GC_WORD - 1u);
    for (cursor = start; cursor + SIMRT_GC_WORD <= end; cursor += SIMRT_GC_WORD) {
        void *word;
        memcpy(&word, (const void *)cursor, sizeof(word));
        simrt_gc_mark(word);
    }
}

static void simrt_gc_trace(SimrtGcHeader *block) {
    unsigned char *payload = (unsigned char *)simrt_gc_payload(block);
    size_t words = (size_t)block->size / SIMRT_GC_WORD;

    switch ((simrt_gc_kind)block->kind) {
        case SIMRT_GC_ARRAY_F64:
            /* Doubles are never addresses. */
            break;
        case SIMRT_GC_TEXT_OBJECT:
            /* Header words plus trailing characters. `main` is an interior
             * pointer into this same block (Phase 3 step 2); character bytes
             * are not managed references. Nothing further to chase. */
            break;
        case SIMRT_GC_TEXT_FRAME:
            /* `{ obj, start, length, pos }`. */
            if (words >= 1) {
                void *object;
                memcpy(&object, payload, sizeof(object));
                simrt_gc_mark(object);
            }
            break;
        case SIMRT_GC_ARRAY_TEXT: {
            /* `{ ndims, bounds[2*ndims], SimrtTextFrame *data[] }`. Precise:
             * the payload really is frame pointers, so unset slots (NULL from
             * `calloc`) retain nothing. */
            int64_t ndims;
            size_t first;
            size_t i;
            if (words < 1) {
                break;
            }
            memcpy(&ndims, payload, sizeof(ndims));
            if (ndims < 0 || (uint64_t)ndims > (uint64_t)words) {
                break;
            }
            first = 1u + (size_t)ndims * 2u;
            for (i = first; i < words; i++) {
                void *frame;
                memcpy(&frame, payload + i * SIMRT_GC_WORD, sizeof(frame));
                if (frame != NULL) {
                    simrt_gc_mark(frame);
                }
            }
            break;
        }
        default:
            /* Objects, opaque runtime packs, and int64/ref arrays: any word
             * may be a reference, so treat them all as candidates. */
            simrt_gc_scan_words(payload, payload + words * SIMRT_GC_WORD);
            break;
    }
}

static void simrt_gc_drain(void) {
    for (;;) {
        SimrtGcHeader *block;
        while (g_gc.gray_len > 0) {
            block = g_gc.gray[--g_gc.gray_len];
            simrt_gc_trace(block);
        }
        if (!g_gc.gray_overflow) {
            return;
        }
        g_gc.gray_overflow = 0;
        for (block = g_gc.blocks; block != NULL; block = block->next) {
            if ((block->flags & SIMRT_GC_FLAG_MARK) != 0) {
                simrt_gc_trace(block);
            }
        }
        if (g_gc.gray_len == 0 && !g_gc.gray_overflow) {
            return;
        }
    }
}

/* ------------------------------------------------------------------ */
/* Precise root frames                                                 */
/* ------------------------------------------------------------------ */

void simrt_gc_root_push(void *frame_raw, int64_t nslots) {
    SimrtGcRootFrame *frame = (SimrtGcRootFrame *)frame_raw;
    if (!g_gc.inited) {
        simrt_gc_init();
    }
    if (frame == NULL || nslots < 0 || (uint64_t)nslots > (uint64_t)UINT32_MAX) {
        return;
    }
    frame->prev = g_gc.roots;
    frame->nslots = (uint32_t)nslots;
    frame->pad = 0;
    if (nslots > 0) {
        memset((char *)frame + sizeof(SimrtGcRootFrame), 0, (size_t)nslots * sizeof(void *));
    }
    g_gc.roots = frame;
}

void simrt_gc_root_pop(void *frame_raw) {
    SimrtGcRootFrame *frame = (SimrtGcRootFrame *)frame_raw;
    if (g_gc.roots != frame) {
        fprintf(stderr, "sim runtime: GC root-frame pop mismatch\n");
        fflush(stderr);
        abort();
    }
    g_gc.roots = frame->prev;
}

void *simrt_gc_root_head(void) {
    return g_gc.roots;
}

void simrt_gc_root_set_head(void *frame) {
    g_gc.roots = (SimrtGcRootFrame *)frame;
}

void simrt_gc_defer_collect(void) {
    if (!g_gc.inited) {
        simrt_gc_init();
    }
    /* Safepoint at the start of a multi-alloc helper: previous results already
     * sit in generated root slots, and this helper has not allocated yet. */
    if (g_gc.defer_collect == 0) {
        simrt_gc_maybe_auto_collect();
    }
    g_gc.defer_collect++;
}

void simrt_gc_allow_collect(void) {
    if (g_gc.defer_collect > 0) {
        g_gc.defer_collect--;
    }
}

static void simrt_gc_mark_root_chain(SimrtGcRootFrame *frame) {
    while (frame != NULL) {
        void **slots = (void **)((char *)frame + sizeof(SimrtGcRootFrame));
        uint32_t i;
        for (i = 0; i < frame->nslots; i++) {
            simrt_gc_mark(slots[i]);
        }
        frame = frame->prev;
    }
}

static void simrt_gc_visit_parked_root_head(void *head, void *user) {
    (void)user;
    simrt_gc_mark_root_chain((SimrtGcRootFrame *)head);
}

/* ------------------------------------------------------------------ */
/* Sweep                                                               */
/* ------------------------------------------------------------------ */

static void simrt_gc_sweep(void) {
    SimrtGcHeader **link = &g_gc.blocks;
    SimrtGcHeader *block = g_gc.blocks;

    while (block != NULL) {
        SimrtGcHeader *next = block->next;
        unsigned keep = block->flags
                        & (SIMRT_GC_FLAG_MARK | SIMRT_GC_FLAG_PINNED | SIMRT_GC_FLAG_UNSWEPT);
        if (keep != 0u) {
            block->flags &= ~(uint32_t)SIMRT_GC_FLAG_MARK;
            link = &block->next;
        } else {
            size_t size = (size_t)block->size;
            size_t poison = size;
            if (!g_gc.poison_all && size > SIM_GC_POISON_BYTES) {
                poison = SIM_GC_POISON_BYTES;
            }
            *link = next;
            g_gc.block_count--;
            g_gc.live_bytes -= size;
            g_gc.blocks_freed++;
            g_gc.bytes_freed += (uint64_t)size;
            /* Poison the leading payload words so a dangling reference fails
             * loudly (bad class_id) instead of reading plausible data. The
             * header survives: `size` is what makes the block reusable, and
             * the object ABI reads it from `obj - 8`. */
            memset(simrt_gc_payload(block), 0xdd, poison);
            block->flags = 0u;
            if (g_gc.free_count < SIMRT_GC_FREE_MAX) {
                block->next = g_gc.free_list;
                g_gc.free_list = block;
                g_gc.free_count++;
            } else {
                free(block);
            }
        }
        block = next;
    }
}

/* ------------------------------------------------------------------ */
/* Collection                                                          */
/* ------------------------------------------------------------------ */

static uint64_t simrt_gc_now_ns(void) {
#if defined(_WIN32)
    static LARGE_INTEGER freq;
    LARGE_INTEGER now;
    if (freq.QuadPart == 0) {
        (void)QueryPerformanceFrequency(&freq);
        if (freq.QuadPart == 0) {
            return 0;
        }
    }
    (void)QueryPerformanceCounter(&now);
    return (uint64_t)((now.QuadPart * 1000000000ull) / (uint64_t)freq.QuadPart);
#else
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
#endif
}

void simrt_gc_collect(void) {
    SimrtGcHeader *block;
    uint64_t started;

    if (!g_gc.inited) {
        simrt_gc_init();
    }
    if (g_gc.disabled || g_gc.collecting) {
        return;
    }
    g_gc.since_collect = 0;
    if (g_gc.blocks == NULL) {
        return;
    }
    if (!simrt_gc_index_build()) {
        g_gc.disabled = 1;
        return;
    }

    g_gc.collecting = 1;
    started = simrt_gc_now_ns();
    g_gc.gray_len = 0;
    g_gc.gray_overflow = 0;
    for (block = g_gc.blocks; block != NULL; block = block->next) {
        block->flags &= ~(uint32_t)SIMRT_GC_FLAG_MARK;
    }

    /* Blocks that are never swept are permanent, so trace from them as roots
     * rather than relying on something else reaching them first. */
    for (block = g_gc.blocks; block != NULL; block = block->next) {
        if ((block->flags & (SIMRT_GC_FLAG_UNSWEPT | SIMRT_GC_FLAG_PINNED)) != 0) {
            simrt_gc_mark(simrt_gc_payload(block));
        }
    }

    simrt_gc_visit_runtime_roots(simrt_gc_mark);
    simrt_seq_gc_visit_roots(simrt_gc_mark);
    simrt_ref_gc_visit_roots(simrt_gc_mark);

    simrt_gc_mark_root_chain(g_gc.roots);
    simrt_coro_gc_visit_parked_root_heads(simrt_gc_visit_parked_root_head, NULL);

    simrt_gc_drain();
    simrt_gc_sweep();

    g_gc.collecting = 0;
    g_gc.since_collect = 0;
    g_gc.collections++;
    {
        uint64_t ended = simrt_gc_now_ns();
        if (ended > started) {
            g_gc.pause_ns += ended - started;
        }
    }
}

/* ------------------------------------------------------------------ */
/* Controls and accounting                                             */
/* ------------------------------------------------------------------ */

void simrt_gc_disable(void) {
    g_gc.inited = 1;
    g_gc.disabled = 1;
}

int simrt_gc_enabled(void) {
    if (!g_gc.inited) {
        simrt_gc_init();
    }
    return g_gc.disabled ? 0 : 1;
}

int64_t simrt_gc_live_blocks(void) {
    return (int64_t)g_gc.block_count;
}

int64_t simrt_gc_live_bytes(void) {
    return (int64_t)g_gc.live_bytes;
}

int64_t simrt_gc_collections(void) {
    return (int64_t)g_gc.collections;
}

int64_t simrt_gc_blocks_freed(void) {
    return (int64_t)g_gc.blocks_freed;
}

int64_t simrt_gc_bytes_freed(void) {
    return (int64_t)g_gc.bytes_freed;
}

int64_t simrt_gc_allocations(void) {
    return (int64_t)g_gc.allocations;
}

int64_t simrt_gc_slots_reused(void) {
    return (int64_t)g_gc.slots_reused;
}

int64_t simrt_gc_pause_ns(void) {
    return (int64_t)g_gc.pause_ns;
}

int64_t simrt_ref_pin(simrt_gc_ptr ref) {
    int i;
    if (ref == NULL) {
        return 0;
    }
    if (!g_gc.inited) {
        simrt_gc_init();
    }
    for (i = 1; i < SIMRT_REF_PIN_MAX; i++) {
        if (g_ref_pins[i] == NULL) {
            g_ref_pins[i] = ref;
            return (int64_t)i;
        }
    }
    fprintf(stderr, "sim runtime: too many pinned refs\n");
    fflush(stderr);
    abort();
}

void simrt_ref_unpin(int64_t id) {
    if (id <= 0 || id >= SIMRT_REF_PIN_MAX) {
        return;
    }
    g_ref_pins[id] = NULL;
}

simrt_gc_ptr simrt_ref_get(int64_t id) {
    if (id <= 0 || id >= SIMRT_REF_PIN_MAX) {
        return NULL;
    }
    return g_ref_pins[id];
}

static void simrt_ref_gc_visit_roots(simrt_gc_mark_fn mark) {
    int i;
    for (i = 1; i < SIMRT_REF_PIN_MAX; i++) {
        if (g_ref_pins[i] != NULL) {
            mark(g_ref_pins[i]);
        }
    }
}
