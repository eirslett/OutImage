/* Stack switching for Simula quasi-parallel sequencing. See coro.c. */

#ifndef SIMRT_CORO_H
#define SIMRT_CORO_H

typedef struct simrt_coro simrt_coro;
typedef void (*simrt_coro_entry)(void *);

/* The coroutine standing for the thread that first asked, created on demand. */
simrt_coro *simrt_coro_main(void);

/* The coroutine currently executing. */
simrt_coro *simrt_coro_current(void);

/* True if `coro` is the OS context actually running (Windows fiber identity,
 * otherwise the same as `simrt_coro_current()`). */
int simrt_coro_is_os_current(const simrt_coro *coro);

/* A coroutine that will run `entry(arg)` on its own stack. It does not start
 * until the first switch into it, and its stack is released by
 * `simrt_coro_destroy`. */
simrt_coro *simrt_coro_create(simrt_coro_entry entry, void *arg);

/* Suspends `from` and continues `to`; returns when something switches back to
 * `from`. Switching into a coroutine whose entry has returned is a hard error. */
void simrt_coro_switch(simrt_coro *from, simrt_coro *to);

/* True once the entry function has returned. */
int simrt_coro_is_done(const simrt_coro *coro);

void simrt_coro_destroy(simrt_coro *coro);

/* --- Garbage collection support ---
 *
 * A parked component's frames hold Simula references in precise root frames
 * (the same linked list generated code pushes while running). On switch, the
 * outgoing coroutine saves the head and the incoming one restores it. The
 * collector walks the running list plus every parked head. */

/* Head of a parked coroutine's precise GC root-frame list. */
typedef void (*simrt_coro_root_head_visitor)(void *head, void *user);

/* Visits every coroutine's saved root-frame head *except* the running one,
 * whose list the collector already holds. */
void simrt_coro_gc_visit_parked_root_heads(simrt_coro_root_head_visitor visit, void *user);

#endif /* SIMRT_CORO_H */
