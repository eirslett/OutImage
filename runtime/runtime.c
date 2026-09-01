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

#ifdef _WIN32
/* Cranelift exports `sim_main`; PE needs CRT startup, so provide `main`. */
int sim_main(void);

int main(void) {
    return sim_main();
}
#endif
