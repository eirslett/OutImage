#include <stdio.h>
#include <stdlib.h>

#include "internal.h"

/* User-facing runtime failures exit with status 1 (not abort) so hosts can
 * distinguish Standard-required errors from internal invariant violations. */
void simrt_error(const char *message) {
    /* A collection triggered from an `atexit` handler (or from anything the
     * shutdown path still allocates) would walk a heap nobody is maintaining
     * any more, so stop collecting before unwinding. */
    simrt_gc_disable();
    fprintf(stderr, "sim: %s\n", message);
    exit(1);
}

/* Explicit C-runtime roots. Frame
 * locals live in Cranelift-emitted precise root frames; SIMSET rings and
 * object fields are reached by the heap trace. These are the references that
 * live only in C globals, where no frame list would find them.
 *
 * Deliberately *not* here: nothing is closed, flushed, or otherwise touched —
 * marking a file object must have no observable effect (plan non-goal
 * "finalization semantics of any kind"). */
void simrt_gc_visit_runtime_roots(simrt_gc_mark_fn mark) {
    simrt_basicio_gc_visit_roots(mark);
    simrt_sim_gc_visit_roots(mark);
}

/* Cranelift exports `sim_main`. Provide `main` only when the CRT is the
 * process entry: PE (always) and sanitizer-linked ELF (`SIMRT_NEED_CRT_MAIN`
 * from the extra `simrt_san` archive). Unsanitized Linux still uses
 * `-nostartfiles --entry=sim_main`. C fixtures compile this file with their
 * own `main`, so this must stay gated. */
#if defined(_WIN32) || defined(SIMRT_NEED_CRT_MAIN)
int sim_main(void);

int main(void) {
    return sim_main();
}
#endif

#if !defined(_WIN32)
/* GNU ld + `-nostartfiles` still pulls libc `atexit`, which wants this CRT
 * symbol. Must be a real object (not NULL) or FILE* teardown SIGSEGVs.
 * Weak so C fixtures / sanitizer links keep crtbegin's definition. */
static char simrt_dso_handle_storage;
void *__dso_handle __attribute__((weak, visibility("hidden"))) =
    &simrt_dso_handle_storage;
#endif
