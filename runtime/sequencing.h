/* Simula quasi-parallel sequencing (Standard chapter 7). See sequencing.c. */

#ifndef SIMRT_SEQUENCING_H
#define SIMRT_SEQUENCING_H

#include "coro.h"

typedef struct simrt_system simrt_system;
typedef struct simrt_component simrt_component;

/* 7.1 states of execution. */
typedef enum {
    SIMRT_STATE_ATTACHED = 0,
    SIMRT_STATE_DETACHED = 1,
    SIMRT_STATE_RESUMED = 2,
    SIMRT_STATE_TERMINATED = 3
} simrt_state;

/* 7.2: entering a subblock or prefixed block that declares a class creates a
 * quasi-parallel system, whose main component is the block instance itself.
 * `block` identifies the block in the source, so that a generator can name the
 * system head that declares its class without the handle being threaded down
 * through every intervening object and procedure. */
simrt_system *simrt_seq_system_enter(long long block);
void simrt_seq_system_exit(simrt_system *system);

/* The system of the outermost block, which chapter 11 makes a prefixed block
 * and 7.2 therefore makes "the outermost system". Created on first use so the
 * generated program does not have to thread it through a prologue. */
simrt_system *simrt_seq_outermost_system(void);

/* Prepares an object's component without running it, and registers it under
 * the object so later statements can name it by object reference alone.
 * `declaring_block` is the system head declaring the class, or 0 when the class
 * is declared in a class body or a procedure body -- such objects can only ever
 * be independent components (7.2), so `resume` on them is an error.
 *
 * Creation is separate from starting because the body may suspend immediately,
 * and a `detach` down there must already be able to find its component. */
simrt_component *simrt_seq_object_create(
    long long declaring_block,
    simrt_coro_entry body,
    void *object
);

/* Runs the body attached to the generating block instance (7.1); returns once
 * the body detaches or terminates. */
void simrt_seq_object_start(simrt_component *component);

/* The remaining operations take the Simula object reference: a `detach` names
 * an object, and `call` / `resume` take a reference expression whose static
 * qualification may be a superclass, so an object-keyed lookup avoids giving
 * the component a per-class field offset. */

/* Notes a prefixed block instance, which has a detach attribute without being
 * an object (7.3.1). Nothing else may be done to it. */
void simrt_seq_block_instance(void *object);

/* 7.3.1 */
void simrt_seq_detach(void *object);
/* 7.3.2 */
void simrt_seq_call(void *object);
/* 7.3.3 */
void simrt_seq_resume(void *object);
/* 7.3.4: the PSC passing through an object's final end. Does not return. */
void simrt_seq_terminate(void *object);

/* Chapter 12 needs a step chapter 7 has no single operation for: the active
 * process reaches its final end and the *next* process in the sequencing set
 * becomes active, rather than control going back to the main component. That is
 * a terminate with respect to `self` composed with a resume of `target`, which
 * has to happen as one switch because a terminated component cannot be switched
 * out of a second time. Does not return. */
void simrt_seq_terminate_resuming(void *self, void *target);

simrt_component *simrt_seq_component_of(void *object);
simrt_state simrt_seq_state(const simrt_component *component);

/* The Simula object whose component head is the current coroutine, or NULL
 * when the running coroutine is the main/adopted thread (MAIN). */
void *simrt_seq_current_object(void);

#endif /* SIMRT_SEQUENCING_H */
