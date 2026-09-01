/* Empty C-runtime / sequencing root visitors for fixtures that compile
 * `gc.c` without `runtime.c` / `sequencing.c`. `gc_collect`
 * calls both; tests that never collect still have to satisfy the linker. */

#include "../../../runtime/gc.h"

void simrt_gc_visit_runtime_roots(simrt_gc_mark_fn mark) {
    (void)mark;
}
