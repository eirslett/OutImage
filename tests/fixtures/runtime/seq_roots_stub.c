/* Empty sequencing-root visitor for fixtures that compile `gc.c` without
 * `sequencing.c`. */

#include "../../../runtime/gc.h"

void simrt_seq_gc_visit_roots(simrt_gc_mark_fn mark) {
    (void)mark;
}
