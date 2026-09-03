#include <stdlib.h>
#include <string.h>

#include "internal.h"
#include "sequencing.h"

/* Simulation / SQS MVP (Standard Ch.12) — statement-index scheduling.
 *
 * MAIN is the sentinel pointer `(void *)1`. Process objects are their
 * allocated object pointers. Fibers are not used; MIR resumes class bodies
 * via `__simrt_coro_pc` after `hold` / `activate` updates this queue.
 */

#define SIMRT_RT_MAIN ((void *)(intptr_t)1)
#define SIMRT_RT_SQS_INITIAL_CAP 16
#define SIMRT_RT_SQS_MAX_LEN 65536

typedef struct {
    double evtime;
    void *process;
    int64_t seq;
} SimrtEventNotice;

typedef struct {
    int active;
    /* Head of the sequencing set: the process that *should* be active. */
    void *current;
    /* The component physically executing, which trails `current` across a
     * scheduling operation until the transfer completes. */
    void *running;
    SimrtEventNotice *sqs;
    size_t len;
    size_t cap;
    int64_t next_seq;
} SimrtSimState;

static SimrtSimState g_sim;

static void simrt_sim_ensure_active(void) {
    if (!g_sim.active) {
        simrt_error("hold/activate/time requires an active Simulation");
    }
}

static void simrt_sim_grow(void) {
    if (g_sim.cap >= SIMRT_RT_SQS_MAX_LEN) {
        simrt_error("SQS length limit exceeded");
    }
    size_t new_cap = g_sim.cap == 0 ? SIMRT_RT_SQS_INITIAL_CAP : g_sim.cap * 2;
    if (new_cap > SIMRT_RT_SQS_MAX_LEN) {
        new_cap = SIMRT_RT_SQS_MAX_LEN;
    }
    SimrtEventNotice *next =
        (SimrtEventNotice *)simrt_host_realloc_n(g_sim.sqs, new_cap, sizeof(SimrtEventNotice));
    if (next == NULL) {
        simrt_error("out of memory growing SQS");
    }
    g_sim.sqs = next;
    g_sim.cap = new_cap;
}

static void simrt_sim_cancel_unlocked(void *process) {
    size_t out = 0;
    for (size_t i = 0; i < g_sim.len; i++) {
        if (g_sim.sqs[i].process != process) {
            g_sim.sqs[out++] = g_sim.sqs[i];
        }
    }
    g_sim.len = out;
}

static int64_t simrt_sim_next_seq(void) {
    int64_t seq = g_sim.next_seq;
    g_sim.next_seq += 1;
    return seq;
}

/* Insert (or replace) an event. `prior != 0` places equal times earlier. */
static void simrt_sim_insert_event(double evtime, void *process, int prior) {
    simrt_sim_cancel_unlocked(process);
    if (g_sim.len >= SIMRT_RT_SQS_MAX_LEN) {
        simrt_error("SQS length limit exceeded");
    }
    if (g_sim.len == g_sim.cap) {
        simrt_sim_grow();
    }
    SimrtEventNotice notice;
    notice.evtime = evtime;
    notice.process = process;
    notice.seq = simrt_sim_next_seq();

    size_t idx = g_sim.len;
    if (prior) {
        for (size_t i = 0; i < g_sim.len; i++) {
            if (g_sim.sqs[i].evtime > evtime || g_sim.sqs[i].evtime == evtime) {
                idx = i;
                break;
            }
        }
    } else {
        for (size_t i = 0; i < g_sim.len; i++) {
            if (g_sim.sqs[i].evtime > evtime) {
                idx = i;
                break;
            }
        }
    }
    for (size_t i = g_sim.len; i > idx; i--) {
        g_sim.sqs[i] = g_sim.sqs[i - 1];
    }
    g_sim.sqs[idx] = notice;
    g_sim.len += 1;
}

static int simrt_sim_is_scheduled(void *process) {
    for (size_t i = 0; i < g_sim.len; i++) {
        if (g_sim.sqs[i].process == process) {
            return 1;
        }
    }
    return 0;
}

static void simrt_sim_advance_current(void) {
    if (g_sim.len == 0) {
        g_sim.current = NULL;
    } else {
        g_sim.current = g_sim.sqs[0].process;
    }
}

void simrt_sim_begin(void) {
    if (g_sim.active) {
        simrt_error("nested Simulation is not supported");
    }
    memset(&g_sim, 0, sizeof(g_sim));
    g_sim.active = 1;
    g_sim.next_seq = 1;
    simrt_sim_insert_event(0.0, SIMRT_RT_MAIN, 1);
    g_sim.current = SIMRT_RT_MAIN;
    g_sim.running = SIMRT_RT_MAIN;
}

void simrt_sim_end(void) {
    free(g_sim.sqs);
    memset(&g_sim, 0, sizeof(g_sim));
}

int64_t simrt_sim_is_active(void) {
    return g_sim.active ? 1 : 0;
}

double simrt_sim_time(void) {
    simrt_sim_ensure_active();
    if (g_sim.len == 0) {
        return 0.0;
    }
    return g_sim.sqs[0].evtime;
}

/* 12.2's CURRENT is the active process: the one holding the PSC, not whichever
 * notice happens to head the sequencing set. */
int64_t simrt_sim_is_main_current(void) {
    simrt_sim_ensure_active();
    return (g_sim.running == NULL || g_sim.running == SIMRT_RT_MAIN) ? 1 : 0;
}

void *simrt_sim_current(void) {
    simrt_sim_ensure_active();
    /* Windows fibers can leave `g_sim.running` naming MAIN while a Process
     * body is physically executing. `current` / post-transfer `this` must
     * follow the OS fiber (simtst96: `into(been)` saw none while the car's
     * been field was still a Head). */
    void *object = simrt_seq_current_object();
    if (object != NULL) {
        return object;
    }
    return g_sim.running == NULL ? SIMRT_RT_MAIN : g_sim.running;
}

void *simrt_sim_main(void) {
    return SIMRT_RT_MAIN;
}

int64_t simrt_sim_idle(void *process) {
    simrt_sim_ensure_active();
    if (process == NULL) {
        return 1;
    }
    return simrt_sim_is_scheduled(process) ? 0 : 1;
}

int64_t simrt_sim_terminated(void *process) {
    simrt_sim_ensure_active();
    if (process == NULL || process == SIMRT_RT_MAIN) {
        return 0;
    }
    simrt_component *component = simrt_seq_component_of(process);
    return component != NULL && simrt_seq_state(component) == SIMRT_STATE_TERMINATED;
}

double simrt_sim_evtime(void *process) {
    simrt_sim_ensure_active();
    if (process == NULL) {
        return 0.0;
    }
    for (size_t i = 0; i < g_sim.len; i++) {
        if (g_sim.sqs[i].process == process) {
            return g_sim.sqs[i].evtime;
        }
    }
    simrt_error("evtime of idle process");
    return 0.0;
}

/* §12.1 nextev: next process in the SQS after `process`, or none if idle/last.
 *
 * MAIN can be operative after a detach-to-main_park even when it has no event
 * notice (Windows fibers). `current.nextev` in simtst96 then has to see the
 * rest of the set so remaining cars can drain with `stop` set. */
void *simrt_sim_nextev(void *process) {
    simrt_sim_ensure_active();
    if (process == NULL) {
        return NULL;
    }
    for (size_t i = 0; i < g_sim.len; i++) {
        if (g_sim.sqs[i].process == process) {
            if (i + 1 < g_sim.len) {
                return g_sim.sqs[i + 1].process;
            }
            return NULL;
        }
    }
    if (process == SIMRT_RT_MAIN && g_sim.len > 0) {
        if (g_sim.sqs[0].process == SIMRT_RT_MAIN) {
            return g_sim.len > 1 ? g_sim.sqs[1].process : NULL;
        }
        return g_sim.sqs[0].process;
    }
    return NULL;
}

static void *simrt_sim_self(void) {
    /* Prefer the object whose stack is actually executing. `g_sim.running` is
     * updated on SQS transfers; a Process body entered through chapter-7
     * attach (or a fiber switch that did not take that path) can still be
     * physically running while `running` still names MAIN. hold/passivate
     * must cancel/reschedule that body, not MAIN (simtst96). */
    void *object = simrt_seq_current_object();
    if (object != NULL) {
        return object;
    }
    return g_sim.running == NULL ? SIMRT_RT_MAIN : g_sim.running;
}

void simrt_sim_hold(double dt) {
    simrt_sim_ensure_active();
    /* 12.3 reschedules *the active process* -- the one executing this hold --
     * which is not always the head of the set: an `activate ... prior` can file
     * a notice ahead of it without taking the PSC away. */
    void *self = simrt_sim_self();
    double now = simrt_sim_time();
    double delay = dt < 0.0 ? 0.0 : dt;
    simrt_sim_insert_event(now + delay, self, 0);
    simrt_sim_advance_current();
}

void simrt_sim_activate_direct(void *process) {
    simrt_sim_ensure_active();
    /* Simula: `activate none` is a no-op. */
    if (process == NULL) {
        return;
    }
    if (simrt_sim_is_scheduled(process)) {
        return;
    }
    /* 12.2 direct activation: "the event notice is inserted in front of the one
     * currently at the lower end of the sequencing set and X becomes active".
     * The caller follows this with a transfer, which is what suspends the
     * formerly active process. */
    double now = simrt_sim_time();
    simrt_sim_insert_event(now, process, 1);
    simrt_sim_advance_current();
}

/* mode: 0 = delay (schedule at time+max(t,0)), 1 = at (schedule at max(t,time)).
 * prior != 0 uses prior ordering; reac != 0 allows already-scheduled processes. */
void simrt_sim_activate_timed(
    void *process,
    double t,
    int64_t mode,
    int64_t prior,
    int64_t reac
) {
    simrt_sim_ensure_active();
    /* Simula: `activate none …` is a no-op. */
    if (process == NULL) {
        return;
    }
    if (!reac && simrt_sim_is_scheduled(process)) {
        return;
    }
    double now = simrt_sim_time();
    double at;
    if (mode == 0) {
        double delay = t < 0.0 ? 0.0 : t;
        at = now + delay;
    } else {
        at = t < now ? now : t;
    }
    if (at <= now && prior) {
        simrt_sim_insert_event(now, process, 1);
    } else {
        simrt_sim_insert_event(at < now ? now : at, process, prior ? 1 : 0);
    }
    simrt_sim_advance_current();
}

/* Insert process at the same time as `other`, immediately before (before!=0)
 * or after it in the SQS. No-op if `other` is not scheduled. */
void simrt_sim_activate_relative(void *process, void *other, int64_t before) {
    simrt_sim_ensure_active();
    if (process == NULL || other == NULL) {
        return;
    }
    if (process == other) {
        return;
    }
    size_t y_pos = (size_t)-1;
    double y_time = 0.0;
    for (size_t i = 0; i < g_sim.len; i++) {
        if (g_sim.sqs[i].process == other) {
            y_pos = i;
            y_time = g_sim.sqs[i].evtime;
            break;
        }
    }
    if (y_pos == (size_t)-1) {
        return;
    }
    simrt_sim_cancel_unlocked(process);
    if (g_sim.len == g_sim.cap) {
        simrt_sim_grow();
    }
    size_t insert_at = before ? y_pos : y_pos + 1;
    /* Re-find y after cancel in case process==other was impossible anyway. */
    for (size_t i = 0; i < g_sim.len; i++) {
        if (g_sim.sqs[i].process == other) {
            insert_at = before ? i : i + 1;
            y_time = g_sim.sqs[i].evtime;
            break;
        }
    }
    /* `activate main after X`: run every other same-time peer of X before
     * MAIN so a later tied winner can still assign `h` (simtst96). Ordinary
     * `activate P after Q` stays immediately after Q (simtst97). */
    if (!before && process == SIMRT_RT_MAIN) {
        while (insert_at < g_sim.len && g_sim.sqs[insert_at].evtime == y_time) {
            insert_at += 1;
        }
    }
    SimrtEventNotice notice;
    notice.evtime = y_time;
    notice.process = process;
    notice.seq = simrt_sim_next_seq();
    for (size_t i = g_sim.len; i > insert_at; i--) {
        g_sim.sqs[i] = g_sim.sqs[i - 1];
    }
    g_sim.sqs[insert_at] = notice;
    g_sim.len += 1;
    simrt_sim_advance_current();
}

/* Chapter 12 scheduling expressed as chapter 7 transfers.
 *
 * The active process is the one whose event notice is first in the sequencing
 * set, so every operation that reorders the set ends by making that process
 * operative. Each process (and MAIN, the SIMULATION block instance) is a
 * component of the SIMULATION system, so becoming operative is a resume, and
 * yielding to MAIN is a detach.
 *
 * `g_sim.current` is the head of the set; `g_sim.running` is the component
 * physically executing. They differ only inside these transfers. */

static void *simrt_sim_head(void) {
    return g_sim.len == 0 ? NULL : g_sim.sqs[0].process;
}

void simrt_sim_transfer_to_head(void) {
    simrt_sim_ensure_active();
    void *head = simrt_sim_head();
    void *running = g_sim.running == NULL ? SIMRT_RT_MAIN : g_sim.running;
    g_sim.current = head;
    if (head == running) {
        return;
    }
    g_sim.running = head == NULL ? SIMRT_RT_MAIN : head;
    if (head == NULL || head == SIMRT_RT_MAIN) {
        /* Nothing left to run, or MAIN's turn: the running process becomes
         * non-operative with its reactivation point after the operation, which
         * is what a detach with respect to it does. */
        if (running != SIMRT_RT_MAIN) {
            simrt_seq_detach(running);
        }
        return;
    }
    simrt_seq_resume(head);
}

/* The active process reaches its final end: it leaves the sequencing set and
 * the next process takes over. Does not return. */
void simrt_sim_terminate_current(void *process) {
    simrt_sim_ensure_active();
    simrt_sim_cancel_unlocked(process);
    void *head = simrt_sim_head();
    g_sim.current = head;
    g_sim.running = head == NULL ? SIMRT_RT_MAIN : head;
    if (head == NULL || head == SIMRT_RT_MAIN) {
        simrt_seq_terminate(process);
        return;
    }
    simrt_seq_terminate_resuming(process, head);
}

void simrt_sim_passivate(void) {
    simrt_sim_ensure_active();
    void *self = simrt_sim_self();
    simrt_sim_cancel_unlocked(self);
    simrt_sim_advance_current();
}

void simrt_sim_cancel(void *process) {
    simrt_sim_ensure_active();
    if (process == NULL) {
        return;
    }
    simrt_sim_cancel_unlocked(process);
    simrt_sim_advance_current();
}

void simrt_sim_finish_main(void) {
    simrt_sim_ensure_active();
    simrt_sim_cancel_unlocked(SIMRT_RT_MAIN);
    simrt_sim_advance_current();
}

int64_t simrt_sim_has_current(void) {
    simrt_sim_ensure_active();
    return g_sim.len > 0 ? 1 : 0;
}

/* SIMSET circular doubly-linked lists (Standard Ch.12). Linkage objects store
 * SUC at offset 8 and PRED at offset 16 (immediately after class_id). Head
 * class_id is registered so suc/pred/first/last can filter Head vs Link. */

#define SIMRT_SIMSET_SUC_OFF 8
#define SIMRT_SIMSET_PRED_OFF 16

static int64_t g_simset_head_class_id = -1;

void simrt_simset_set_head_class_id(int64_t class_id) {
    g_simset_head_class_id = class_id;
}

static void *simset_load(void *obj, int64_t offset) {
    if (obj == NULL) {
        return NULL;
    }
    return *(void **)((unsigned char *)obj + (size_t)offset);
}

static void simset_store(void *obj, int64_t offset, void *value) {
    if (obj == NULL) {
        return;
    }
    *(void **)((unsigned char *)obj + (size_t)offset) = value;
}

static int simset_is_head(void *obj) {
    if (obj == NULL || g_simset_head_class_id < 0) {
        return 0;
    }
    return *(int64_t *)obj == g_simset_head_class_id;
}

static int simset_is_link(void *obj) {
    return obj != NULL && !simset_is_head(obj);
}

void simrt_simset_init_head(void *head) {
    if (head == NULL) {
        return;
    }
    simset_store(head, SIMRT_SIMSET_SUC_OFF, head);
    simset_store(head, SIMRT_SIMSET_PRED_OFF, head);
}

void simrt_simset_out(void *x) {
    if (x == NULL) {
        return;
    }
    void *suc = simset_load(x, SIMRT_SIMSET_SUC_OFF);
    if (suc == NULL) {
        return;
    }
    void *pred = simset_load(x, SIMRT_SIMSET_PRED_OFF);
    if (suc != NULL) {
        simset_store(suc, SIMRT_SIMSET_PRED_OFF, pred);
    }
    if (pred != NULL) {
        simset_store(pred, SIMRT_SIMSET_SUC_OFF, suc);
    }
    simset_store(x, SIMRT_SIMSET_SUC_OFF, NULL);
    simset_store(x, SIMRT_SIMSET_PRED_OFF, NULL);
}

void simrt_simset_precede(void *x, void *ptr) {
    if (x == NULL) {
        return;
    }
    /* Standard §12.3: precede(none) ≡ out (detach from set). */
    simrt_simset_out(x);
    if (ptr == NULL) {
        return;
    }
    void *ptr_suc = simset_load(ptr, SIMRT_SIMSET_SUC_OFF);
    if (ptr_suc == NULL && !simset_is_head(ptr)) {
        return;
    }
    void *pred = simset_load(ptr, SIMRT_SIMSET_PRED_OFF);
    simset_store(x, SIMRT_SIMSET_SUC_OFF, ptr);
    simset_store(x, SIMRT_SIMSET_PRED_OFF, pred);
    if (pred != NULL) {
        simset_store(pred, SIMRT_SIMSET_SUC_OFF, x);
    }
    simset_store(ptr, SIMRT_SIMSET_PRED_OFF, x);
}

void simrt_simset_follow(void *x, void *ptr) {
    if (x == NULL) {
        return;
    }
    /* Standard §12.3: follow(none) ≡ out (detach from set). */
    simrt_simset_out(x);
    if (ptr == NULL) {
        return;
    }
    void *ptr_suc = simset_load(ptr, SIMRT_SIMSET_SUC_OFF);
    /* Not in a set (and not a Head ring): follow has no further effect. */
    if (ptr_suc == NULL && !simset_is_head(ptr)) {
        return;
    }
    simset_store(x, SIMRT_SIMSET_PRED_OFF, ptr);
    simset_store(x, SIMRT_SIMSET_SUC_OFF, ptr_suc);
    if (ptr_suc != NULL) {
        simset_store(ptr_suc, SIMRT_SIMSET_PRED_OFF, x);
    }
    simset_store(ptr, SIMRT_SIMSET_SUC_OFF, x);
}

void simrt_simset_into(void *x, void *head) {
    /* into(S) ≡ precede(S) for Head S — insert as last member. */
    simrt_simset_precede(x, head);
}

/* Skip SQS-busy Processes in suc/first walks so wait-queues don't return
 * cars that already resumed and are holding (simtst96 send/put). */
static int simrt_simset_busy_process(void *x) {
    if (!g_sim.active || x == NULL) {
        return 0;
    }
    return simrt_sim_is_scheduled(x);
}

void *simrt_simset_suc(void *x) {
    void *suc = simset_load(x, SIMRT_SIMSET_SUC_OFF);
    int guard = 0;
    while (simset_is_link(suc) && simrt_simset_busy_process(suc)) {
        suc = simset_load(suc, SIMRT_SIMSET_SUC_OFF);
        if (++guard > 65536) {
            return NULL;
        }
    }
    return simset_is_link(suc) ? suc : NULL;
}

void *simrt_simset_pred(void *x) {
    void *pred = simset_load(x, SIMRT_SIMSET_PRED_OFF);
    int guard = 0;
    while (simset_is_link(pred) && simrt_simset_busy_process(pred)) {
        pred = simset_load(pred, SIMRT_SIMSET_PRED_OFF);
        if (++guard > 65536) {
            return NULL;
        }
    }
    return simset_is_link(pred) ? pred : NULL;
}

void *simrt_simset_first(void *head) {
    return simrt_simset_suc(head);
}

void *simrt_simset_last(void *head) {
    return simrt_simset_pred(head);
}

int64_t simrt_simset_empty(void *head) {
    if (head == NULL) {
        return 1;
    }
    void *suc = simset_load(head, SIMRT_SIMSET_SUC_OFF);
    return (suc == NULL || suc == head) ? 1 : 0;
}

int64_t simrt_simset_cardinal(void *head) {
    int64_t count = 0;
    void *ptr = simrt_simset_suc(head);
    while (simset_is_link(ptr)) {
        count += 1;
        ptr = simrt_simset_suc(ptr);
    }
    return count;
}

void simrt_sim_gc_visit_roots(simrt_gc_mark_fn mark) {
    size_t i;
    if (mark == NULL) {
        return;
    }
    if (g_sim.current != SIMRT_RT_MAIN) {
        mark(g_sim.current);
    }
    if (g_sim.running != SIMRT_RT_MAIN) {
        mark(g_sim.running);
    }
    for (i = 0; i < g_sim.len; i++) {
        void *process = g_sim.sqs[i].process;
        if (process != SIMRT_RT_MAIN) {
            mark(process);
        }
    }
}
