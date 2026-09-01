#include <stdio.h>
#include <stdlib.h>

#include "internal.h"

/* Object / class support.
 *
 * Layout of the pointer handed to generated code: bytes [0, 8) hold `class_id`
 * (int64); integer attributes follow at successive 8-byte offsets. The eight
 * bytes *before* that pointer hold the payload size, so field load/store can
 * bounds-check offsets at parity with the interpreter.
 * Those eight bytes are the trailing
 * `size` member of `SimrtGcHeader`, which is why that member has to stay
 * last: nothing may `free` an object pointer or treat it as an allocation
 * base. Objects live on the managed heap and are reclaimed once nothing
 * reachable refers to them. Field load/store through a null (`none`)
 * reference abort, matching the interpreter's "remote access through none
 * reference" error. */
void *simrt_object_alloc(int64_t size, int64_t class_id) {
    if (size < (int64_t)sizeof(int64_t)) {
        fprintf(stderr, "sim: invalid object size %lld\n", (long long)size);
        abort();
    }
    if ((uint64_t)size > (uint64_t)(SIZE_MAX - sizeof(SimrtGcHeader))) {
        simrt_error("object size overflow");
    }
    /* Class ids start at 1, so 0 marks the runtime's own record types (the
     * SYSIN / SYSOUT packs below), which carry no Simula attributes. */
    void *obj = simrt_gc_alloc(
        class_id == 0 ? SIMRT_GC_OPAQUE : SIMRT_GC_OBJECT, size
    );
    if (obj == NULL) {
        fprintf(stderr, "sim: out of memory allocating object (size %lld)\n", (long long)size);
        abort();
    }
    *(int64_t *)obj = class_id;
    return obj;
}

static int64_t simrt_object_size(const void *obj) {
    return *(const int64_t *)((const unsigned char *)obj - sizeof(int64_t));
}

int64_t simrt_object_class_id(void *obj) {
    if (obj == NULL) {
        simrt_error("remote access through none reference");
    }
    return *(int64_t *)obj;
}

int64_t simrt_object_class_id_safe(void *obj) {
    if (obj == NULL || obj == (void *)(intptr_t)1) {
        return -1;
    }
    return *(int64_t *)obj;
}

int64_t simrt_object_load_i64(void *obj, int64_t offset) {
    if (obj == NULL) {
        simrt_error("remote access through none reference");
    }
    int64_t obj_size = simrt_object_size(obj);
    if (!simrt_object_offset_ok(obj_size, offset)) {
        simrt_error("object field offset out of range");
    }
    return *(int64_t *)((unsigned char *)obj + (size_t)offset);
}

void simrt_object_store_i64(void *obj, int64_t offset, int64_t value) {
    if (obj == NULL) {
        simrt_error("remote assignment through none reference");
    }
    int64_t obj_size = simrt_object_size(obj);
    if (!simrt_object_offset_ok(obj_size, offset)) {
        simrt_error("object field offset out of range");
    }
    *(int64_t *)((unsigned char *)obj + (size_t)offset) = value;
}
