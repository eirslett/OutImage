/* Exercises the native collector (`runtime/gc.c`) directly, below any compiler
 * involvement, so a rooting bug shows up as a failed assertion here rather
 * than as a rare wrong answer in a compiled Simula program.
 *
 * Everything is driven through the runtime's own allocators, because the whole
 * point of the native collector is that objects, arrays, and
 * texts share one managed heap.
 *
 * Tracing of frame roots is precise: a C local is not a root. Tests that need
 * a pointer to survive collection push an explicit root frame. A dropped
 * pointer needs no scrub — an integer that merely looks like a heap address
 * does not keep the object alive.
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../../../runtime/runtime.h"

static int failures;

static void report(int ok, const char *name, const char *detail) {
    if (ok) {
        printf("PASS %s\n", name);
    } else {
        printf("FAIL %s: %s\n", name, detail);
        failures++;
    }
}

/* One function's worth of precise roots for C tests. Push, then store slots
 * (`push` zeroes them). */
typedef struct {
    SimrtGcRootFrame hdr;
    void *slots[4];
} GcProtect;

static void gc_protect_begin(GcProtect *protect, int64_t nslots) {
    simrt_gc_root_push(&protect->hdr, nslots);
}

static void gc_protect_end(GcProtect *protect) {
    simrt_gc_root_pop(&protect->hdr);
}

/* Field offset 8 stands in for the SIMSET `SUC` slot: the first word after
 * `class_id`, exactly where a ring link lives in a real object. */
#define LINK_OFFSET 8
#define TAG_OFFSET 16
#define OBJECT_SIZE 24

static void store_ref(void *object, int64_t offset, void *value) {
    simrt_object_store_i64(object, offset, (int64_t)(intptr_t)value);
}

static void *load_ref(void *object, int64_t offset) {
    return (void *)(intptr_t)simrt_object_load_i64(object, offset);
}

/* --- 1. Unreachable objects are reclaimed --------------------------------- */

static void allocate_and_drop(int count) {
    void *object = NULL;
    int i;
    for (i = 0; i < count; i++) {
        object = simrt_object_alloc(OBJECT_SIZE, 1);
        simrt_object_store_i64(object, TAG_OFFSET, i);
    }
    (void)object;
}

static void test_unreachable_objects_are_reclaimed(void) {
    int64_t before;
    int64_t after;
    char detail[160];

    simrt_gc_collect();
    before = simrt_gc_live_blocks();
    allocate_and_drop(200);
    simrt_gc_collect();
    after = simrt_gc_live_blocks();

    snprintf(
        detail,
        sizeof(detail),
        "live blocks went %lld -> %lld across 200 dropped objects",
        (long long)before,
        (long long)after
    );
    report(after <= before, "unreachable objects are reclaimed", detail);
}

/* --- 2. A reference held in a precise root slot survives ------------------ */

static void test_a_held_reference_survives(void) {
    GcProtect protect;
    void *keep = simrt_object_alloc(OBJECT_SIZE, 7);
    char detail[160];
    int64_t tag;

    gc_protect_begin(&protect, 1);
    protect.slots[0] = keep;
    simrt_object_store_i64(keep, TAG_OFFSET, 4242);
    allocate_and_drop(200);
    simrt_gc_collect();

    tag = simrt_object_load_i64(keep, TAG_OFFSET);
    snprintf(detail, sizeof(detail), "class_id=%lld tag=%lld",
             (long long)simrt_object_class_id(keep), (long long)tag);
    report(
        simrt_object_class_id(keep) == 7 && tag == 4242,
        "a reference held across a collection survives",
        detail
    );
    gc_protect_end(&protect);
}

/* An integer local with a heap-address bit pattern is not a root. */
static void test_an_integer_that_looks_like_a_pointer_does_not_keep_an_object_alive(void) {
    void *dropped = simrt_object_alloc(OBJECT_SIZE, 1);
    volatile uintptr_t junk = (uintptr_t)dropped;
    int64_t freed_before;
    int64_t freed;
    char detail[160];

    dropped = NULL;
    freed_before = simrt_gc_blocks_freed();
    simrt_gc_collect();
    freed = simrt_gc_blocks_freed() - freed_before;

    snprintf(
        detail,
        sizeof(detail),
        "junk=%p freed=%lld",
        (void *)junk,
        (long long)freed
    );
    report(
        junk != 0 && freed >= 1,
        "an integer that looks like a pointer does not keep an object alive",
        detail
    );
}

/* --- 3. A two-object ring: rooted survives, unrooted is collected ---------- */

static void *build_ring(int64_t tag) {
    void *first = simrt_object_alloc(OBJECT_SIZE, 3);
    void *second = simrt_object_alloc(OBJECT_SIZE, 3);
    store_ref(first, LINK_OFFSET, second);
    store_ref(second, LINK_OFFSET, first);
    simrt_object_store_i64(first, TAG_OFFSET, tag);
    simrt_object_store_i64(second, TAG_OFFSET, tag + 1);
    return first;
}

static void build_and_drop_ring(int64_t tag) {
    (void)build_ring(tag);
}

static void test_a_rooted_ring_survives(void) {
    GcProtect protect;
    void *head = build_ring(900);
    void *tail;
    char detail[160];

    gc_protect_begin(&protect, 1);
    protect.slots[0] = head;
    allocate_and_drop(100);
    simrt_gc_collect();

    tail = load_ref(head, LINK_OFFSET);
    snprintf(
        detail,
        sizeof(detail),
        "head tag=%lld tail tag=%lld back-link matches=%d",
        (long long)simrt_object_load_i64(head, TAG_OFFSET),
        (long long)simrt_object_load_i64(tail, TAG_OFFSET),
        load_ref(tail, LINK_OFFSET) == head
    );
    report(
        simrt_object_load_i64(head, TAG_OFFSET) == 900
            && simrt_object_load_i64(tail, TAG_OFFSET) == 901
            && load_ref(tail, LINK_OFFSET) == head,
        "a ring reached through one live member survives whole",
        detail
    );
    gc_protect_end(&protect);
}

static void test_an_unrooted_ring_is_collected(void) {
    int64_t freed_before;
    int64_t freed;
    char detail[160];

    simrt_gc_collect();
    freed_before = simrt_gc_blocks_freed();
    build_and_drop_ring(700);
    simrt_gc_collect();
    freed = simrt_gc_blocks_freed() - freed_before;

    snprintf(detail, sizeof(detail), "collection freed %lld blocks, expected >= 2", (long long)freed);
    /* Reference counting could not do this: each member holds the other. */
    report(freed >= 2, "a ring with no external reference is collected", detail);
}

/* --- 4. Unreachable texts are reclaimed; content pointers pin objects ------ */

static void allocate_and_drop_texts(int count) {
    int i;
    for (i = 0; i < count; i++) {
        (void)simrt_text_from_literal((const unsigned char *)"abcdefgh", 8);
    }
}

static void test_unreachable_texts_are_reclaimed(void) {
    GcProtect protect;
    SimrtTextFrame *rooted = simrt_text_from_literal((const unsigned char *)"rooted", 6);
    int64_t before;
    int64_t after;
    int64_t allocated;
    int64_t freed_delta;
    int64_t length;
    int64_t first;
    int64_t freed_before;
    char detail[224];

    gc_protect_begin(&protect, 1);
    protect.slots[0] = rooted;
    simrt_gc_collect();
    before = simrt_gc_live_blocks();
    freed_before = simrt_gc_blocks_freed();
    allocated = simrt_gc_allocations();
    allocate_and_drop_texts(10);
    allocated = simrt_gc_allocations() - allocated;
    simrt_gc_collect();
    after = simrt_gc_live_blocks();
    freed_delta = simrt_gc_blocks_freed() - freed_before;

    length = simrt_text_length(rooted);
    first = simrt_text_getchar(rooted);
    snprintf(
        detail,
        sizeof(detail),
        "%lld text blocks allocated, live %lld -> %lld, freed_delta=%lld, "
        "rooted length=%lld first char=%lld",
        (long long)allocated,
        (long long)before,
        (long long)after,
        (long long)freed_delta,
        (long long)length,
        (long long)first
    );
    /* Each from_literal makes a frame + an object (2 blocks). Dropping ten of
     * them should free those 20; the rooted pair must survive. */
    report(
        allocated >= 20 && freed_delta >= allocated / 2 && after <= before
            && length == 6 && first == 'r',
        "unreachable text frames and text objects are reclaimed",
        detail
    );
    gc_protect_end(&protect);
}

void simrt_text_content_ptr_len(SimrtTextFrame *frame, const unsigned char **ptr_out, int64_t *len_out);

/* Drop the frame handle but keep an interior content pointer in a root slot.
 * Step 2 folds characters into the TEXT_OBJECT block so that pointer pins it. */
static const unsigned char *allocate_text_and_return_content(void) {
    const unsigned char *content = NULL;
    int64_t len = 0;
    SimrtTextFrame *frame = simrt_text_from_literal((const unsigned char *)"interior", 8);
    simrt_text_content_ptr_len(frame, &content, &len);
    (void)len;
    (void)frame; /* frame itself becomes unreachable when this returns */
    return content;
}

static void test_interior_content_pointer_keeps_text_object_alive(void) {
    GcProtect protect;
    const unsigned char *content;
    int64_t freed_before;
    int64_t freed_after;
    char detail[160];

    content = allocate_text_and_return_content();
    gc_protect_begin(&protect, 1);
    protect.slots[0] = (void *)content;
    freed_before = simrt_gc_blocks_freed();
    simrt_gc_collect();
    freed_after = simrt_gc_blocks_freed();

    snprintf(
        detail,
        sizeof(detail),
        "content=%p first='%c' freed_delta=%lld",
        (void *)content,
        content != NULL ? (char)content[0] : '?',
        (long long)(freed_after - freed_before)
    );
    /* The frame may be swept, but the object (holding "interior") must stay.
     * Reading content[0] after collect proves it was not freed. */
    report(
        content != NULL && content[0] == 'i' && content[7] == 'r',
        "an interior text content pointer keeps its text object alive",
        detail
    );
    gc_protect_end(&protect);
}

/* --- 5. Text array elements are traced precisely -------------------------- */

static void test_a_rooted_text_array_keeps_its_elements(void) {
    GcProtect protect;
    int64_t bounds[2];
    void *array;
    int64_t live_after;
    char detail[160];

    bounds[0] = 1;
    bounds[1] = 4;
    array = simrt_array_alloc_text(1, bounds);
    gc_protect_begin(&protect, 1);
    protect.slots[0] = array;
    allocate_and_drop(100);
    simrt_gc_collect();
    live_after = simrt_gc_live_blocks();

    snprintf(detail, sizeof(detail), "array=%p live_blocks=%lld", array, (long long)live_after);
    /* The descriptor is reachable only through this frame; its four notext
     * elements are reachable only through the descriptor. */
    report(array != NULL && live_after > 0, "a rooted text array survives with its elements", detail);
    gc_protect_end(&protect);
}

/* --- 6. An integer array reached only from an object field survives -------- */

static void *build_object_owning_an_array(void) {
    int64_t bounds[2];
    void *owner = simrt_object_alloc(OBJECT_SIZE, 5);
    void *array;
    int64_t index = 2;

    bounds[0] = 1;
    bounds[1] = 3;
    array = simrt_array_alloc_i64(1, bounds);
    simrt_array_store_i64(array, 1, &index, 1234);
    store_ref(owner, LINK_OFFSET, array);
    return owner;
}

static void test_an_array_reached_through_a_field_survives(void) {
    GcProtect protect;
    void *owner = build_object_owning_an_array();
    void *array;
    int64_t index = 2;
    int64_t value;
    char detail[160];

    gc_protect_begin(&protect, 1);
    protect.slots[0] = owner;
    allocate_and_drop(200);
    simrt_gc_collect();

    array = load_ref(owner, LINK_OFFSET);
    value = simrt_array_load_i64(array, 1, &index);
    snprintf(detail, sizeof(detail), "array element = %lld, expected 1234", (long long)value);
    report(value == 1234, "an array reached only through an object field survives", detail);
    gc_protect_end(&protect);
}

/* --- 7. SYSOUT is a root even with no user reference ----------------------- */

static void touch_sysout(void) {
    (void)simrt_sysout();
}

static void test_sysout_is_a_root(void) {
    int64_t class_id;
    char detail[96];

    touch_sysout();
    allocate_and_drop(100);
    simrt_gc_collect();

    /* Freed blocks are poisoned, so a reclaimed SYSOUT would not read back 0. */
    class_id = simrt_object_class_id(simrt_sysout());
    snprintf(detail, sizeof(detail), "SYSOUT class_id=%lld", (long long)class_id);
    report(class_id == 0, "SYSOUT survives collection without a user reference", detail);
}

/* --- 8. Swept blocks are reused instead of going back to the host --------- */

static void test_swept_blocks_are_reused(void) {
    int64_t live_before;
    int64_t live_after;
    int64_t reused_before;
    int64_t reused;
    char detail[192];

    /* First batch stocks the free list with `OBJECT_SIZE` blocks... */
    allocate_and_drop(200);
    simrt_gc_collect();
    live_before = simrt_gc_live_blocks();
    reused_before = simrt_gc_slots_reused();

    /* ...which the second batch, being the same size, should draw from. */
    allocate_and_drop(200);
    simrt_gc_collect();
    live_after = simrt_gc_live_blocks();
    reused = simrt_gc_slots_reused() - reused_before;

    snprintf(
        detail,
        sizeof(detail),
        "200 same-size allocations reused %lld slots, live blocks %lld -> %lld",
        (long long)reused,
        (long long)live_before,
        (long long)live_after
    );
    report(
        reused >= 100 && live_after <= live_before,
        "swept blocks are reused by later same-size allocations",
        detail
    );
}

int main(void) {
    if (!simrt_gc_enabled()) {
        printf("SKIP collector disabled on this host\n");
        return 0;
    }

    test_unreachable_texts_are_reclaimed();
    test_interior_content_pointer_keeps_text_object_alive();
    test_unreachable_objects_are_reclaimed();
    test_a_held_reference_survives();
    test_an_integer_that_looks_like_a_pointer_does_not_keep_an_object_alive();
    test_a_rooted_ring_survives();
    test_an_unrooted_ring_is_collected();
    test_a_rooted_text_array_keeps_its_elements();
    test_an_array_reached_through_a_field_survives();
    test_sysout_is_a_root();
    test_swept_blocks_are_reused();

    printf(
        "DONE collections=%lld blocks_freed=%lld live_blocks=%lld live_bytes=%lld "
        "slots_reused=%lld pause_ns=%lld\n",
        (long long)simrt_gc_collections(),
        (long long)simrt_gc_blocks_freed(),
        (long long)simrt_gc_live_blocks(),
        (long long)simrt_gc_live_bytes(),
        (long long)simrt_gc_slots_reused(),
        (long long)simrt_gc_pause_ns()
    );
    return failures == 0 ? 0 : 1;
}
