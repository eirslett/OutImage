/* The annotated example from Standard 7.4, hand-translated onto the sequencing
 * runtime. It is the Standard's own worked case and exercises every rule that
 * matters: a detach from inside a procedure, a nested system, a resume, a
 * detach of an *outer* object performed from inside an inner object's stack
 * (figure 7.7), and both flavours of final end.
 *
 *   1  begin comment S1;
 *   2        ref(C1) X1;
 *   3        class C1;
 *   4        begin procedure P1; detach;
 *   5              P1
 *   6        end C1;
 *   7        ref(C2) X2;
 *   8        class C2;
 *   9        begin procedure P2;
 *  10              begin detach;
 *  11                      ! - see fig. 7.7;
 *  12              end P2;
 *  13              begin comment system S2;
 *  14                    ref(C3) X3;
 *  15                    class C3;
 *  16                    begin detach;
 *  17                          P2
 *  18                    end C3;
 *  19                    X3:- new C3;
 *  20                    resume(X3)
 *  21              end S2
 *  22        end C2;
 *  23        X1:- new C1;
 *  24        X2:- new C2;
 *  25        call(X2)
 *  26  end S1;
 */

#include <stdio.h>
#include <string.h>

#include "../../../runtime/sequencing.h"

/* Source identities of the three system head blocks (line 1, line 13, and the
 * inner subblock of the second program). The runtime resolves a generator to
 * the instance of the named block it is running inside. */
#define BLOCK_S1 1
#define BLOCK_S2 13
#define BLOCK_INNER 103

static char trace[256];

static void note(const char *what) { strncat(trace, what, sizeof(trace) - strlen(trace) - 1); }

static simrt_system *S1;
static simrt_system *S2;
/* Stand-ins for the Simula objects; the runtime keys components by object. */
static int X1_obj, X2_obj, X3_obj;
static simrt_component *X1;
static simrt_component *X2;
static simrt_component *X3;

/* Line 4: an attribute of C1, so its bare `detach` detaches X1. */
static void P1(void) {
    note("p1");
    simrt_seq_detach(&X1_obj);
}

static void C1_body(void *arg) {
    (void)arg;
    note("c1");
    P1();
    simrt_seq_terminate(&X1_obj);
}

/* Lines 9-12: an attribute of C2, so its bare `detach` detaches X2 -- even
 * though by the time it runs the PSC is inside X3 (figure 7.7). */
static void P2(void) {
    note("p2");
    simrt_seq_detach(&X2_obj);
    note("q");
}

static void C3_body(void *arg) {
    (void)arg;
    note("c3");
    simrt_seq_detach(&X3_obj);
    note("k");
    P2();
    simrt_seq_terminate(&X3_obj);
}

static void C2_body(void *arg) {
    (void)arg;
    note("c2");
    /* Line 13: a subblock declaring class C3, hence a system head. */
    S2 = simrt_seq_system_enter(BLOCK_S2);
    note("s2");
    X3 = simrt_seq_object_create(BLOCK_S2, C3_body, &X3_obj);
    simrt_seq_object_start(X3);
    note("g");
    simrt_seq_resume(&X3_obj);
    note("r");
    simrt_seq_system_exit(S2);
    note("e2");
    simrt_seq_terminate(&X2_obj);
}

/* Second program: the SIMSET shape from the corpus, where the *inner* subblock
 * is the system head, so a resumed object's final end returns after the resume
 * rather than out to the program block.
 *
 *   SIMSET begin ref(A) x; Link class A;
 *     begin "A"; begin ref(C) y; class C; begin "C"; detach; "E" end;
 *                      "B"; y :- new C; "D"; resume(y); "F" end;
 *           "G" end;
 *     "AA"; x :- new A; "AB" end
 */
static char inner_trace[64];

static void inner_note(const char *what) {
    strncat(inner_trace, what, sizeof(inner_trace) - strlen(inner_trace) - 1);
}

static simrt_system *inner_system;
static int inner_a_obj, inner_c_obj;
static simrt_component *inner_a;
static simrt_component *inner_c;

static void inner_C_body(void *arg) {
    (void)arg;
    inner_note("C");
    simrt_seq_detach(&inner_c_obj);
    inner_note("E");
    simrt_seq_terminate(&inner_c_obj);
}

static void inner_A_body(void *arg) {
    (void)arg;
    inner_note("A");
    inner_system = simrt_seq_system_enter(BLOCK_INNER);
    inner_note("B");
    inner_c = simrt_seq_object_create(BLOCK_INNER, inner_C_body, &inner_c_obj);
    simrt_seq_object_start(inner_c);
    inner_note("D");
    simrt_seq_resume(&inner_c_obj);
    inner_note("F");
    simrt_seq_system_exit(inner_system);
    inner_note("G");
    simrt_seq_terminate(&inner_a_obj);
}

static void run_inner_system_program(void) {
    inner_note("AA");
    inner_a = simrt_seq_object_create(BLOCK_S1, inner_A_body, &inner_a_obj);
    simrt_seq_object_start(inner_a);
    inner_note("AB");
}

int main(void) {
    /* Line 1: the outermost block declares C1 and C2. */
    S1 = simrt_seq_system_enter(BLOCK_S1);

    X1 = simrt_seq_object_create(BLOCK_S1, C1_body, &X1_obj);
    simrt_seq_object_start(X1);
    note("1");

    X2 = simrt_seq_object_create(BLOCK_S1, C2_body, &X2_obj);
    simrt_seq_object_start(X2);
    note("2");

    simrt_seq_call(&X2_obj);
    note("3");

    if (simrt_seq_state(X2) != SIMRT_STATE_TERMINATED) {
        note("?");
    }
    if (simrt_seq_state(X3) != SIMRT_STATE_TERMINATED) {
        note("?");
    }
    /* X1 detached in P1 and was never called or resumed again. */
    if (simrt_seq_state(X1) != SIMRT_STATE_DETACHED) {
        note("?");
    }

    printf("%s\n", trace);

    run_inner_system_program();
    printf("%s\n", inner_trace);

    simrt_seq_system_exit(S1);
    return 0;
}
