/* Standalone exercise for runtime/coro.c: symmetric switching, deep frames
 * across a suspend, and termination. Built by tests/runtime_coro.rs. */

#include <stdio.h>
#include <string.h>

#include "../../../runtime/coro.h"

static char trace[256];

static void note(const char *what) { strncat(trace, what, sizeof(trace) - strlen(trace) - 1); }

static simrt_coro *main_coro;
static simrt_coro *a_coro;
static simrt_coro *b_coro;

/* A nested call that suspends: the Standard's reactivation chain includes the
 * object's procedure activations, so `deep` must still be on the stack, with
 * its locals intact, after the switch away and back. */
static long deep(long seed) {
    long local = seed * 7;
    note("d");
    simrt_coro_switch(a_coro, main_coro);
    note("D");
    return local + seed;
}

static void a_entry(void *arg) {
    (void)arg;
    note("a");
    simrt_coro_switch(a_coro, main_coro);
    note("A");
    long got = deep(6);
    if (got != 48) {
        note("!");
    }
    note("z");
}

static void b_entry(void *arg) {
    (void)arg;
    note("b");
    /* Symmetric transfer straight to another component, not back to main. */
    simrt_coro_switch(b_coro, a_coro);
    note("B");
}

int main(void) {
    main_coro = simrt_coro_main();
    a_coro = simrt_coro_create(a_entry, NULL);
    b_coro = simrt_coro_create(b_entry, NULL);

    note("1");
    simrt_coro_switch(main_coro, a_coro); /* a, back here */
    note("2");
    simrt_coro_switch(main_coro, b_coro); /* b -> A, d, back here */
    note("3");
    simrt_coro_switch(main_coro, a_coro); /* D, z, a terminates */
    note("4");

    if (!simrt_coro_is_done(a_coro)) {
        note("?");
    }
    /* b is still parked inside its switch to a. */
    if (simrt_coro_is_done(b_coro)) {
        note("?");
    }

    printf("%s\n", trace);
    simrt_coro_destroy(a_coro);
    simrt_coro_destroy(b_coro);
    return 0;
}
