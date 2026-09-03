/* Simula quasi-parallel sequencing: chapter 7 of the Standard, expressed
 * directly over the stack-switching primitive in coro.c.
 *
 * The one idea that shapes everything here is the *reactivation point*. A
 * non-operative component's reactivation point need not sit in its own head:
 * if object X calls procedure P, or generates an attached object Z, and the
 * suspending statement runs down there, then X's reactivation point is inside
 * that activation, and reactivating X must continue there with the whole chain
 * intact (7.2, "reactivation chain"). Each component therefore records the
 * coroutine holding its reactivation point -- `park` -- which is the coroutine
 * that was running when it was suspended, not necessarily its own.
 */

#include <stdio.h>
#include <stdlib.h>

#include "gc.h"
#include "host.h"
#include "sequencing.h"

struct simrt_system {
    /* Reactivation point of the main component. The main component's head is
     * the system head block instance, so this starts as the coroutine entering
     * the block and moves only when the main component suspends. */
    simrt_coro *main_park;
    /* The operative component of the system; NULL means the main component,
     * which is operative "initially ... and only component of the system". */
    simrt_component *operative;
};

struct simrt_component {
    /* The object's own coroutine, i.e. the component head. */
    simrt_coro *head;
    /* Where this component's reactivation point currently lives. */
    simrt_coro *park;
    /* Where control returns when an *attached* object detaches (7.3.1 case 1):
     * the block instance it is attached to. */
    simrt_coro *attached_to;
    /* System of the block instance declaring the class, or NULL for objects
     * that can only be independent components (7.2). */
    simrt_system *system;
    simrt_state state;
    /* The Simula object this component belongs to; see the lookup below. */
    void *object;
    /* The coroutine that generated this object. A class is always generated
     * from within the block instance that declares it, or from inside another
     * object generated there, so following this chain walks outwards through
     * the block instances whose systems are visible here. */
    simrt_coro *origin;
    /* A prefixed block instance is a block instance with class attributes, so
     * it has a detach attribute but is not an object: 7.3.1 opens by saying
     * such a detach has no effect. It is registered here only so that a detach
     * reaching it can be told apart from one naming an object that never
     * became a component, which is an error. */
    int block_instance;
    struct simrt_component *next_in_bucket;
    struct simrt_component *next_component;
};

/* Sequencing bugs show up as control landing in the wrong object several
 * transfers later, so SIM_SEQ_TRACE=1 logs each transfer as it happens. */
static int simrt_seq_tracing(void) {
    static int state = -1;
    if (state < 0) {
        const char *flag = getenv("SIM_SEQ_TRACE");
        state = flag != NULL && flag[0] != '\0' && flag[0] != '0';
    }
    return state;
}

/* Objects are named by reference throughout chapter 7, and a reference's static
 * qualification may be a superclass, so the component is found by object
 * identity rather than through a field at a class-dependent offset. */
#define SIMRT_SEQ_BUCKETS 1021u

static simrt_component *simrt_seq_buckets[SIMRT_SEQ_BUCKETS];
/* Every component, for the reverse lookup from a coroutine to its object. */
static simrt_component *simrt_seq_all;

static unsigned simrt_seq_bucket_of(const void *object) {
    uintptr_t bits = (uintptr_t)object;
    /* Object addresses are at least 8-byte aligned; drop the dead low bits. */
    return (unsigned)((bits >> 3) % SIMRT_SEQ_BUCKETS);
}

static void simrt_seq_register(simrt_component *component) {
    unsigned bucket = simrt_seq_bucket_of(component->object);
    component->next_in_bucket = simrt_seq_buckets[bucket];
    simrt_seq_buckets[bucket] = component;
    component->next_component = simrt_seq_all;
    simrt_seq_all = component;
}

static simrt_component *simrt_seq_component_running_on(const simrt_coro *coro) {
    for (simrt_component *it = simrt_seq_all; it != NULL; it = it->next_component) {
        if (it->head == coro) {
            return it;
        }
    }
    return NULL;
}

simrt_component *simrt_seq_component_of(void *object) {
    if (object == NULL) {
        return NULL;
    }
    for (simrt_component *it = simrt_seq_buckets[simrt_seq_bucket_of(object)]; it != NULL;
         it = it->next_in_bucket) {
        if (it->object == object) {
            return it;
        }
    }
    return NULL;
}

void *simrt_seq_current_object(void) {
    simrt_component *component = simrt_seq_component_running_on(simrt_coro_current());
    if (component != NULL) {
        return component->object;
    }
    /* Windows fibers share TLS: `simrt_coro_current_ptr` can still name MAIN
     * while a Process body is executing on its fiber. Match the OS context. */
    for (simrt_component *it = simrt_seq_all; it != NULL; it = it->next_component) {
        if (it->block_instance) {
            continue;
        }
        if (simrt_coro_is_os_current(it->head) || simrt_coro_is_os_current(it->park)) {
            return it->object;
        }
    }
    return NULL;
}

static void simrt_seq_error(const char *message) {
    fprintf(stderr, "sim runtime: %s\n", message);
    fflush(stderr);
    abort();
}

static const char *simrt_seq_state_name(simrt_state state) {
    switch (state) {
        case SIMRT_STATE_ATTACHED:
            return "attached";
        case SIMRT_STATE_DETACHED:
            return "detached";
        case SIMRT_STATE_RESUMED:
            return "resumed";
        default:
            return "terminated";
    }
}

/* Active system head block instances, innermost last. A generator names the
 * block that declares the class by a compile-time id, and the system it must
 * report to is the instance of that block it is executing inside -- which is
 * why a frame also records the coroutine that entered it: two objects of the
 * same class each running their own copy of an inner system head produce two
 * frames with equal ids, and only the owning coroutine tells them apart. */
typedef struct {
    long long block;
    simrt_system *system;
    simrt_coro *owner;
} simrt_seq_frame;

/* Fixed §0.5.2 implementation limit on simultaneously active quasi-parallel
 * systems; exceeding it reports a clear error rather than truncating. */
#define SIMRT_SEQ_MAX_SYSTEMS 256u

static simrt_seq_frame simrt_seq_frames[SIMRT_SEQ_MAX_SYSTEMS];
static unsigned simrt_seq_frame_count;

simrt_system *simrt_seq_system_enter(long long block) {
    simrt_system *system = (simrt_system *)simrt_host_calloc(1, sizeof(simrt_system));
    if (system == NULL) {
        simrt_seq_error("out of memory entering a quasi-parallel system");
    }
    system->main_park = simrt_coro_current();
    system->operative = NULL;
    if (simrt_seq_frame_count == SIMRT_SEQ_MAX_SYSTEMS) {
        simrt_seq_error("too many quasi-parallel systems active at once");
    }
    simrt_seq_frames[simrt_seq_frame_count].block = block;
    simrt_seq_frames[simrt_seq_frame_count].system = system;
    simrt_seq_frames[simrt_seq_frame_count].owner = simrt_coro_current();
    simrt_seq_frame_count++;
    return system;
}

void simrt_seq_system_exit(simrt_system *system) {
    /* Blocks nest, but coroutines interleave, so the frame being left is not
     * necessarily the newest one. */
    for (unsigned i = simrt_seq_frame_count; i > 0; i--) {
        if (simrt_seq_frames[i - 1].system != system) {
            continue;
        }
        for (unsigned j = i; j < simrt_seq_frame_count; j++) {
            simrt_seq_frames[j - 1] = simrt_seq_frames[j];
        }
        simrt_seq_frame_count--;
        break;
    }
    free(system);
}

/* The system of the instance of `block` this coroutine is executing inside. */
static simrt_system *simrt_seq_system_for_block(long long block) {
    if (block == 0) {
        /* A class declared in a class body or a procedure body has no system
         * head, so its objects can only ever be independent components. */
        return NULL;
    }
    for (simrt_coro *coro = simrt_coro_current(); coro != NULL;) {
        for (unsigned i = simrt_seq_frame_count; i > 0; i--) {
            if (simrt_seq_frames[i - 1].block == block
                && simrt_seq_frames[i - 1].owner == coro) {
                return simrt_seq_frames[i - 1].system;
            }
        }
        simrt_component *component = simrt_seq_component_running_on(coro);
        coro = component == NULL ? NULL : component->origin;
    }
    /* Not instrumented (an injected system class, say): the outermost system
     * keeps such an object a component rather than failing outright. */
    return simrt_seq_outermost_system();
}

simrt_system *simrt_seq_outermost_system(void) {
    static simrt_system outermost;
    static int ready;
    if (!ready) {
        /* The outermost block instance runs on the coroutine that starts the
         * program, and it is always operating (7.2). */
        outermost.main_park = simrt_coro_main();
        outermost.operative = NULL;
        ready = 1;
    }
    return &outermost;
}

simrt_component *simrt_seq_object_create(
    long long declaring_block,
    simrt_coro_entry body,
    void *object
) {
    simrt_component *component = (simrt_component *)simrt_host_calloc(1, sizeof(simrt_component));
    if (component == NULL) {
        simrt_seq_error("out of memory generating an object");
    }
    component->head = simrt_coro_create(body, object);
    component->park = component->head;
    component->origin = simrt_coro_current();
    component->system = simrt_seq_system_for_block(declaring_block);
    component->state = SIMRT_STATE_ATTACHED;
    component->object = object;
    simrt_seq_register(component);
    return component;
}

void simrt_seq_object_start(simrt_component *component) {
    if (component == NULL) {
        simrt_seq_error("starting no object");
    }
    simrt_coro *generator = simrt_coro_current();
    component->attached_to = generator;
    simrt_coro_switch(generator, component->head);
}

/* Shared by detach (7.3.1) and by an object's final end (7.3.4), which "is the
 * same as that of a detach with respect to that object, except that the object
 * becomes terminated, not detached". */
static void simrt_seq_leave(simrt_component *self, simrt_state next) {
    if (self == NULL) {
        simrt_seq_error("detach with respect to no object");
    }

    simrt_coro *current = simrt_coro_current();
    simrt_coro *target = NULL;

    switch (self->state) {
        case SIMRT_STATE_ATTACHED:
            /* 7.3.1 case 1: control returns to the block instance the object is
             * attached to, immediately after the generator or call statement. */
            target = self->attached_to;
            break;
        case SIMRT_STATE_RESUMED: {
            /* 7.3.1 case 3: control goes to the reactivation point of the main
             * component of the object's system, which thereby becomes
             * operative -- not to whoever resumed this object. */
            simrt_system *system = self->system;
            if (system == NULL) {
                simrt_seq_error("a resumed object must belong to a system");
            }
            system->operative = NULL;
            target = system->main_park;
            break;
        }
        default:
            simrt_seq_error(
                self->state == SIMRT_STATE_DETACHED
                    ? "detach with respect to an object that is already detached"
                    : "detach with respect to a terminated object"
            );
    }

    self->state = next;
    /* A terminated object "attains no reactivation point and loses its status
     * as a component head". */
    self->park = next == SIMRT_STATE_TERMINATED ? NULL : current;
    if (simrt_seq_tracing()) {
        fprintf(
            stderr,
            "seq: %s object=%p head=%p from=%p to=%p\n",
            next == SIMRT_STATE_TERMINATED ? "terminate" : "detach",
            self->object,
            (void *)self->head,
            (void *)current,
            (void *)target
        );
    }
    simrt_coro_switch(current, target);
}

/* Resolves an object reference to its component, rejecting the cases the
 * Standard calls errors before the operation is attempted. */
static simrt_component *simrt_seq_require(void *object, const char *operation) {
    if (object == NULL) {
        char message[64];
        snprintf(message, sizeof(message), "%s(none)", operation);
        simrt_seq_error(message);
    }
    simrt_component *component = simrt_seq_component_of(object);
    if (component == NULL) {
        char message[128];
        snprintf(
            message,
            sizeof(message),
            "%s with respect to an object that never became a component",
            operation
        );
        simrt_seq_error(message);
    }
    return component;
}

void simrt_seq_block_instance(void *object) {
    simrt_component *marker = (simrt_component *)simrt_host_calloc(1, sizeof(simrt_component));
    if (marker == NULL) {
        simrt_seq_error("out of memory entering a prefixed block");
    }
    marker->object = object;
    marker->block_instance = 1;
    marker->state = SIMRT_STATE_ATTACHED;
    simrt_seq_register(marker);
}

void simrt_seq_detach(void *object) {
    simrt_component *self = simrt_seq_require(object, "detach");
    if (self->block_instance) {
        /* 7.3.1: "If X is an instance of a prefixed block the detach statement
         * has no effect." */
        return;
    }
    simrt_seq_leave(self, SIMRT_STATE_DETACHED);
}

void simrt_seq_terminate(void *object) {
    simrt_seq_leave(simrt_seq_require(object, "final end"), SIMRT_STATE_TERMINATED);
    simrt_seq_error("control returned into a terminated object");
}

void simrt_seq_call(void *object) {
    simrt_component *target = simrt_seq_require(object, "call");
    if (target->state != SIMRT_STATE_DETACHED) {
        char message[128];
        snprintf(
            message,
            sizeof(message),
            "call with respect to an object that is %s; 7.3.2 requires a detached object",
            simrt_seq_state_name(target->state)
        );
        simrt_seq_error(message);
    }

    simrt_coro *current = simrt_coro_current();
    /* The callee "becomes attached to the block instance containing the call
     * statement, whereby Y loses its status as a component head". */
    target->state = SIMRT_STATE_ATTACHED;
    target->attached_to = current;
    if (simrt_seq_tracing()) {
        fprintf(
            stderr,
            "seq: call object=%p head=%p from=%p to=%p\n",
            target->object,
            (void *)target->head,
            (void *)current,
            (void *)target->park
        );
    }
    simrt_coro_switch(current, target->park);
}

void simrt_seq_resume(void *object) {
    simrt_component *target = simrt_seq_require(object, "resume");
    if (target->system == NULL) {
        simrt_seq_error(
            "resume with respect to an object that is not local to a system head; "
            "7.3.3 allows it only for objects of a class declared in a subblock or "
            "prefixed block"
        );
    }
    if (target->state == SIMRT_STATE_RESUMED) {
        /* "If Y is a resumed object, the resume statement has no effect." */
        return;
    }
    if (target->state != SIMRT_STATE_DETACHED) {
        char message[128];
        snprintf(
            message,
            sizeof(message),
            "resume with respect to an object that is %s; 7.3.3 requires a detached object",
            simrt_seq_state_name(target->state)
        );
        simrt_seq_error(message);
    }

    simrt_coro *current = simrt_coro_current();
    simrt_system *system = target->system;
    simrt_component *operative = system->operative;

    /* The previously operative component of the system becomes non-operative,
     * with its reactivation point immediately after the resume statement --
     * here, on whichever coroutine is executing it. */
    if (operative == NULL) {
        system->main_park = current;
    } else {
        operative->state = SIMRT_STATE_DETACHED;
        operative->park = current;
    }

    system->operative = target;
    target->state = SIMRT_STATE_RESUMED;
    if (simrt_seq_tracing()) {
        fprintf(
            stderr,
            "seq: resume object=%p head=%p from=%p to=%p main_park=%p\n",
            target->object,
            (void *)target->head,
            (void *)current,
            (void *)target->park,
            (void *)system->main_park
        );
    }
    simrt_coro_switch(current, target->park);
}

void simrt_seq_terminate_resuming(void *self_object, void *target_object) {
    simrt_component *self = simrt_seq_require(self_object, "final end");
    simrt_component *target = simrt_seq_require(target_object, "resume");
    simrt_system *system = target->system;
    if (system == NULL) {
        simrt_seq_error("a scheduled process must belong to a system");
    }
    if (target->state != SIMRT_STATE_DETACHED) {
        char message[128];
        snprintf(
            message,
            sizeof(message),
            "scheduling an object that is %s; a detached object is required",
            simrt_seq_state_name(target->state)
        );
        simrt_seq_error(message);
    }

    simrt_coro *current = simrt_coro_current();
    /* The terminated object "attains no reactivation point and loses its status
     * as a component head", so unlike a resume it leaves no park behind. */
    self->state = SIMRT_STATE_TERMINATED;
    self->park = NULL;
    if (system->operative == self) {
        system->operative = NULL;
    }

    system->operative = target;
    target->state = SIMRT_STATE_RESUMED;
    if (simrt_seq_tracing()) {
        fprintf(
            stderr,
            "seq: terminate-resuming self=%p target=%p from=%p to=%p\n",
            self->object,
            target->object,
            (void *)current,
            (void *)target->park
        );
    }
    simrt_coro_switch(current, target->park);
    simrt_seq_error("control returned into a terminated object");
}

simrt_state simrt_seq_state(const simrt_component *component) {
    return component == NULL ? SIMRT_STATE_TERMINATED : component->state;
}

/* Every object that ever became a component, so an object reachable only
 * through a reactivation chain (7.2) stays live. Components are themselves
 * never freed, which makes this the native counterpart of the interpreter's
 * deliberate `SeqComponent` retention. */
void simrt_seq_gc_visit_roots(simrt_gc_mark_fn mark) {
    if (mark == NULL) {
        return;
    }
    for (simrt_component *it = simrt_seq_all; it != NULL; it = it->next_component) {
        if (it->object != NULL) {
            mark(it->object);
        }
    }
}
