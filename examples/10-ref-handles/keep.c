#include "gc.h"

void *keep(void *r) {
    int64_t id = simrt_ref_pin(r);
    simrt_gc_collect();
    void *got = simrt_ref_get(id);
    simrt_ref_unpin(id);
    return got;
}
