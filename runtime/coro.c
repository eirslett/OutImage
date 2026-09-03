/* Stack switching for Simula quasi-parallel sequencing (Standard chapter 7).
 *
 * A Simula object component keeps a *reactivation point*, and the Standard's
 * reactivation chain explicitly includes the object's nested procedure
 * activations -- the example in 7.4 detaches from inside a procedure and later
 * resumes back into it with the frames intact. That is only expressible if each
 * component owns a real call stack, which is what this file provides.
 *
 * This layer is deliberately Simula-agnostic: it can create a stack, switch to
 * it, and switch away. Which component to switch to, and what "attached" or
 * "detached" mean, belong to the sequencing layer above it.
 *
 * Backends: hand-written register saves on x86-64 / AArch64, Win32 fibers on
 * Windows. Anything else gets a stub that aborts on first use, so hosts without
 * a backend still build and still run programs that never suspend.
 */

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)
#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0601
#endif
#define SIMRT_CORO_FIBERS 1
#include <windows.h>
#elif defined(__x86_64__) || defined(__aarch64__)
#define SIMRT_CORO_ASM 1
#include <sys/mman.h>
#include <unistd.h>
#else
#define SIMRT_CORO_NONE 1
#endif

#include "coro.h"
#include "gc.h"
#include "host.h"

#if defined(__has_feature)
#if __has_feature(address_sanitizer)
#define SIMRT_ASAN 1
#endif
#endif
#if defined(__SANITIZE_ADDRESS__)
#define SIMRT_ASAN 1
#endif

#ifdef SIMRT_ASAN
void __sanitizer_start_switch_fiber(void **fake_stack_save, const void *bottom, size_t size);
void __sanitizer_finish_switch_fiber(void *fake_stack_save, const void **bottom_old, size_t *size_old);
#endif

struct simrt_coro {
    simrt_coro_entry entry;
    void *arg;
    /* Set once the entry function has returned. */
    int done;
    /* Where to continue when the entry function returns without the sequencing
     * layer having switched away first. */
    struct simrt_coro *home;
#if defined(SIMRT_CORO_ASM)
    /* Saved stack pointer while this coroutine is not running. */
    void *sp;
    /* mmap region backing the stack, including the guard page. */
    void *stack_base;
    size_t stack_size;
#elif defined(SIMRT_CORO_FIBERS)
    LPVOID fiber;
    /* The main coroutine adopts the thread rather than creating a fiber. */
    int adopted_thread;
#endif
    /* Precise GC root-frame list head while this coroutine is parked. */
    void *gc_roots;
    /* Every live coroutine, so the collector can walk parked root lists. */
    struct simrt_coro *next_all;
#ifdef SIMRT_ASAN
    /* Usable stack region for AddressSanitizer fiber switching. NULL/0 means
     * the real thread stack (the adopted main coroutine). */
    const void *asan_stack_bottom;
    size_t asan_stack_size;
    void *asan_fake_stack;
#endif
};

static void simrt_coro_panic(const char *message) {
    fprintf(stderr, "sim runtime: %s\n", message);
    fflush(stderr);
    abort();
}

/* `current` is thread-local so an embedder with several host threads does not
 * mix stacks. The all-list and main coroutine are process-wide: Windows fibers
 * share a thread but `__declspec(thread)` / `__thread` can still name MAIN
 * while a Process fiber is running, and a TLS all-list is invisible to GC on
 * the other fiber (simtst96: MAIN's `towns` / `r` were swept, find() rebuilt
 * an empty town, then `r.cars.first.been` none-deref'd). */
#if defined(_MSC_VER)
#define SIMRT_CORO_THREAD_LOCAL __declspec(thread)
#else
#define SIMRT_CORO_THREAD_LOCAL __thread
#endif

static SIMRT_CORO_THREAD_LOCAL simrt_coro *simrt_coro_current_ptr;
static simrt_coro *simrt_coro_main_ptr;
static simrt_coro *simrt_coro_all_ptr;

#if defined(SIMRT_CORO_FIBERS)
/* MSVC's GetCurrentFiber() is 0x1E00 when the thread is not a fiber. */
#define SIMRT_CORO_NOT_A_FIBER ((LPVOID)(uintptr_t)0x1E00)

static simrt_coro *simrt_coro_from_os_fiber(void) {
    LPVOID fiber = GetCurrentFiber();
    simrt_coro *it;
    if (fiber == NULL || fiber == SIMRT_CORO_NOT_A_FIBER) {
        return NULL;
    }
    for (it = simrt_coro_all_ptr; it != NULL; it = it->next_all) {
        if (it->fiber == fiber) {
            return it;
        }
    }
    return NULL;
}
#endif

static void simrt_coro_register(simrt_coro *coro) {
    coro->next_all = simrt_coro_all_ptr;
    simrt_coro_all_ptr = coro;
}

static void simrt_coro_unregister(simrt_coro *coro) {
    simrt_coro **link = &simrt_coro_all_ptr;
    while (*link != NULL) {
        if (*link == coro) {
            *link = coro->next_all;
            coro->next_all = NULL;
            return;
        }
        link = &(*link)->next_all;
    }
}

static size_t simrt_coro_stack_bytes(void) {
    static size_t cached;
    if (cached != 0) {
        return cached;
    }
    size_t kb = 512; /* §0.5.2 default; override SIMRT_CORO_STACK_KB */
    const char *override = getenv("SIMRT_CORO_STACK_KB");
    if (override != NULL && *override != '\0') {
        char *end = NULL;
        unsigned long parsed = strtoul(override, &end, 10);
        if (end != NULL && *end == '\0' && parsed >= 16 && parsed <= (1UL << 20)) {
            kb = (size_t)parsed;
        }
    }
    cached = kb * 1024u;
    return cached;
}

/* Not static: the assembly trampolines below branch to it by symbol name. */
void simrt_coro_run(simrt_coro *coro) {
#if defined(SIMRT_ASAN) && defined(SIMRT_CORO_ASM)
    /* First entry lands here from the trampoline, not from switch_impl's
     * return, so the fake-stack restore has to happen in both places. */
    __sanitizer_finish_switch_fiber(NULL, NULL, NULL);
#endif
    coro->entry(coro->arg);
    coro->done = 1;
    /* The sequencing layer normally switches away before the entry returns.
     * If it did not, fall back to whoever switched in most recently. */
    if (coro->home == NULL) {
        simrt_coro_panic("coroutine finished with nowhere to return to");
    }
    for (;;) {
        simrt_coro_switch(coro, coro->home);
        simrt_coro_panic("switched into a finished coroutine");
    }
}

/* ------------------------------------------------------------------ */
/* x86-64 / AArch64: save callee-saved registers, swap stack pointers. */
/* ------------------------------------------------------------------ */
#if defined(SIMRT_CORO_ASM)

/* Swaps stacks: saves the current context, stores its stack pointer through
 * `save_sp`, then continues on `new_sp`. */
extern void simrt_coro_swap(void **save_sp, void *new_sp);

/* First entry into a fresh coroutine; the coroutine pointer arrives in the
 * callee-saved register the initial frame was primed with. */
extern void simrt_coro_trampoline(void);

#if defined(__APPLE__)
#define SIMRT_SYM(name) "_" #name
#else
#define SIMRT_SYM(name) #name
#endif

#if defined(__x86_64__)

/* SysV: rbx, rbp, r12-r15 are callee-saved; the return address sits on the
 * stack, so `ret` after the swap lands wherever the new context left off. */
__asm__(
    ".text\n"
    ".globl " SIMRT_SYM(simrt_coro_swap) "\n"
    ".p2align 4\n"
    SIMRT_SYM(simrt_coro_swap) ":\n"
    "  pushq %rbp\n"
    "  pushq %rbx\n"
    "  pushq %r12\n"
    "  pushq %r13\n"
    "  pushq %r14\n"
    "  pushq %r15\n"
    "  movq %rsp, (%rdi)\n"
    "  movq %rsi, %rsp\n"
    "  popq %r15\n"
    "  popq %r14\n"
    "  popq %r13\n"
    "  popq %r12\n"
    "  popq %rbx\n"
    "  popq %rbp\n"
    "  ret\n"
);

/* r12 holds the coroutine pointer, planted in the initial frame below. The
 * `and` re-establishes 16-byte alignment so the following call is ABI-legal. */
__asm__(
    ".text\n"
    ".globl " SIMRT_SYM(simrt_coro_trampoline) "\n"
    ".p2align 4\n"
    SIMRT_SYM(simrt_coro_trampoline) ":\n"
    "  movq %r12, %rdi\n"
    "  andq $-16, %rsp\n"
    "  callq " SIMRT_SYM(simrt_coro_run) "\n"
    "  ud2\n"
);

/* Six saved registers then the return address the swap's `ret` will consume.
 * Restore order is r15, r14, r13, r12, rbx, rbp, ret — so r12 is slot 3. */
#define SIMRT_CORO_FRAME_SLOTS 7
#define SIMRT_CORO_ENTRY_SLOT 6
#define SIMRT_CORO_SELF_SLOT 3 /* r12 */

#elif defined(__aarch64__)

/* AAPCS: x19-x28 plus fp/lr and d8-d15 are callee-saved. Restoring lr and
 * returning is what lands in the other context. */
__asm__(
    ".text\n"
    ".globl " SIMRT_SYM(simrt_coro_swap) "\n"
    ".p2align 2\n"
    SIMRT_SYM(simrt_coro_swap) ":\n"
    "  sub sp, sp, #160\n"
    "  stp x19, x20, [sp, #0]\n"
    "  stp x21, x22, [sp, #16]\n"
    "  stp x23, x24, [sp, #32]\n"
    "  stp x25, x26, [sp, #48]\n"
    "  stp x27, x28, [sp, #64]\n"
    "  stp x29, x30, [sp, #80]\n"
    "  stp d8, d9, [sp, #96]\n"
    "  stp d10, d11, [sp, #112]\n"
    "  stp d12, d13, [sp, #128]\n"
    "  stp d14, d15, [sp, #144]\n"
    "  mov x2, sp\n"
    "  str x2, [x0]\n"
    "  mov sp, x1\n"
    "  ldp x19, x20, [sp, #0]\n"
    "  ldp x21, x22, [sp, #16]\n"
    "  ldp x23, x24, [sp, #32]\n"
    "  ldp x25, x26, [sp, #48]\n"
    "  ldp x27, x28, [sp, #64]\n"
    "  ldp x29, x30, [sp, #80]\n"
    "  ldp d8, d9, [sp, #96]\n"
    "  ldp d10, d11, [sp, #112]\n"
    "  ldp d12, d13, [sp, #128]\n"
    "  ldp d14, d15, [sp, #144]\n"
    "  add sp, sp, #160\n"
    "  ret\n"
);

/* x19 holds the coroutine pointer, planted in the initial frame below. */
__asm__(
    ".text\n"
    ".globl " SIMRT_SYM(simrt_coro_trampoline) "\n"
    ".p2align 2\n"
    SIMRT_SYM(simrt_coro_trampoline) ":\n"
    "  mov x0, x19\n"
    "  bl " SIMRT_SYM(simrt_coro_run) "\n"
    "  brk #1\n"
);

/* The 160-byte frame is 20 pointer slots; lr is the second half of the
 * x29/x30 pair at offset 80, i.e. slot 11. */
#define SIMRT_CORO_FRAME_SLOTS 20
#define SIMRT_CORO_ENTRY_SLOT 11
#define SIMRT_CORO_SELF_SLOT 0 /* x19 */

#endif

static void simrt_coro_stack_free(simrt_coro *coro) {
    if (coro->stack_base != NULL) {
        munmap(coro->stack_base, coro->stack_size);
        coro->stack_base = NULL;
    }
}

/* Lays down the frame that `simrt_coro_swap` will restore on the first
 * switch in: zeroed callee-saved slots except the one carrying `coro`, and a
 * return address pointing at the trampoline. */
static int simrt_coro_stack_init(simrt_coro *coro) {
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    size_t usable = simrt_coro_stack_bytes();
    size_t total = usable + page; /* guard page below the low end */

    void *base = mmap(NULL, total, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (base == MAP_FAILED) {
        return 0;
    }
    /* Turn the lowest page into a guard so a stack overflow faults instead of
     * quietly scribbling on another coroutine. */
    if (mprotect(base, page, PROT_NONE) != 0) {
        munmap(base, total);
        return 0;
    }

    coro->stack_base = base;
    coro->stack_size = total;
#ifdef SIMRT_ASAN
    /* Skip the guard page: ASan must not treat PROT_NONE as a valid stack. */
    coro->asan_stack_bottom = (const unsigned char *)base + page;
    coro->asan_stack_size = usable;
    coro->asan_fake_stack = NULL;
#endif

    uintptr_t top = (uintptr_t)base + total;
    top &= ~(uintptr_t)15;
    void **frame = (void **)top - SIMRT_CORO_FRAME_SLOTS;
    memset(frame, 0, SIMRT_CORO_FRAME_SLOTS * sizeof(void *));
    frame[SIMRT_CORO_SELF_SLOT] = coro;
    frame[SIMRT_CORO_ENTRY_SLOT] = (void *)simrt_coro_trampoline;
    coro->sp = frame;
    return 1;
}

static void simrt_coro_switch_impl(simrt_coro *from, simrt_coro *to) {
    simrt_coro_swap(&from->sp, to->sp);
}

static void simrt_coro_adopt_thread(simrt_coro *coro) {
    coro->sp = NULL;
    coro->stack_base = NULL;
    coro->stack_size = 0;
}

/* ------------------------------------------------------------------ */
/* Windows: fibers already are this primitive.                         */
/* ------------------------------------------------------------------ */
#elif defined(SIMRT_CORO_FIBERS)

static VOID CALLBACK simrt_coro_fiber_entry(LPVOID param) {
    simrt_coro *coro = (simrt_coro *)param;
    /* Fiber-local TLS would still name MAIN (copied at CreateFiber). */
    simrt_coro_current_ptr = coro;
    simrt_coro_run(coro);
}

static int simrt_coro_stack_init(simrt_coro *coro) {
    /* FLOAT_SWITCH keeps XMM/x87 with the fiber. Without it, `hold(dist)` can
     * resume with another component's floating state (simtst96 on Windows). */
    coro->fiber = CreateFiberEx(
        0,
        simrt_coro_stack_bytes(),
        FIBER_FLAG_FLOAT_SWITCH,
        simrt_coro_fiber_entry,
        coro
    );
    return coro->fiber != NULL;
}

static void simrt_coro_stack_free(simrt_coro *coro) {
    if (coro->fiber != NULL && !coro->adopted_thread) {
        DeleteFiber(coro->fiber);
    }
    coro->fiber = NULL;
}

static void simrt_coro_switch_impl(simrt_coro *from, simrt_coro *to) {
    (void)from;
    SwitchToFiber(to->fiber);
}

static void simrt_coro_adopt_thread(simrt_coro *coro) {
    coro->adopted_thread = 1;
    coro->fiber = ConvertThreadToFiberEx(NULL, FIBER_FLAG_FLOAT_SWITCH);
    if (coro->fiber == NULL) {
        /* Already a fiber (embedded host); GetCurrentFiber is then valid. */
        coro->fiber = GetCurrentFiber();
    }
}

/* ------------------------------------------------------------------ */
/* No backend: build, but refuse to suspend.                           */
/* ------------------------------------------------------------------ */
#else

static int simrt_coro_stack_init(simrt_coro *coro) {
    (void)coro;
    return 0;
}

static void simrt_coro_stack_free(simrt_coro *coro) { (void)coro; }

static void simrt_coro_switch_impl(simrt_coro *from, simrt_coro *to) {
    (void)from;
    (void)to;
    simrt_coro_panic("quasi-parallel sequencing is not supported on this architecture");
}

static void simrt_coro_adopt_thread(simrt_coro *coro) { (void)coro; }

#endif

/* ------------------------------------------------------------------ */
/* Public interface                                                    */
/* ------------------------------------------------------------------ */

simrt_coro *simrt_coro_main(void) {
    if (simrt_coro_main_ptr == NULL) {
        simrt_coro *coro = (simrt_coro *)simrt_host_calloc(1, sizeof(simrt_coro));
        if (coro == NULL) {
            simrt_coro_panic("out of memory creating the main component");
        }
        simrt_coro_adopt_thread(coro);
        simrt_coro_register(coro);
        simrt_coro_main_ptr = coro;
        simrt_coro_current_ptr = coro;
    }
    return simrt_coro_main_ptr;
}

simrt_coro *simrt_coro_current(void) {
#if defined(SIMRT_CORO_FIBERS)
    {
        simrt_coro *by_fiber = simrt_coro_from_os_fiber();
        if (by_fiber != NULL) {
            simrt_coro_current_ptr = by_fiber;
            return by_fiber;
        }
    }
#endif
    if (simrt_coro_current_ptr == NULL) {
        return simrt_coro_main();
    }
    return simrt_coro_current_ptr;
}

int simrt_coro_is_os_current(const simrt_coro *coro) {
    if (coro == NULL) {
        return 0;
    }
#if defined(SIMRT_CORO_FIBERS)
    /* TLS `current_ptr` is per-thread, not per-fiber. After SwitchToFiber the
     * OS current fiber is the source of truth for which component is running. */
    return coro->fiber != NULL && GetCurrentFiber() == coro->fiber;
#else
    return simrt_coro_current() == coro;
#endif
}

simrt_coro *simrt_coro_create(simrt_coro_entry entry, void *arg) {
    /* Make sure the thread is adoptable before the first switch away from it. */
    simrt_coro_main();

    simrt_coro *coro = (simrt_coro *)simrt_host_calloc(1, sizeof(simrt_coro));
    if (coro == NULL) {
        simrt_coro_panic("out of memory creating a component");
    }
    coro->entry = entry;
    coro->arg = arg;
    if (!simrt_coro_stack_init(coro)) {
        free(coro);
        simrt_coro_panic("could not allocate a component stack");
    }
    simrt_coro_register(coro);
    return coro;
}

void simrt_coro_switch(simrt_coro *from, simrt_coro *to) {
    if (from == NULL || to == NULL) {
        simrt_coro_panic("switch with no component");
    }
    if (from == to) {
        return;
    }
    if (to->done) {
        simrt_coro_panic("switch into a component that has already terminated");
    }
    to->home = from;
    simrt_coro_current_ptr = to;
    from->gc_roots = simrt_gc_root_head();
    simrt_gc_root_set_head(to->gc_roots);
#if defined(SIMRT_ASAN) && defined(SIMRT_CORO_ASM)
    __sanitizer_start_switch_fiber(
        &from->asan_fake_stack, to->asan_stack_bottom, to->asan_stack_size
    );
#endif
    simrt_coro_switch_impl(from, to);
#if defined(SIMRT_ASAN) && defined(SIMRT_CORO_ASM)
    __sanitizer_finish_switch_fiber(from->asan_fake_stack, NULL, NULL);
#endif
    /* Back again: whoever switched to us restored `current` for themselves, so
     * re-establish it for this side. Windows identifies the fiber from the OS
     * in case TLS still names the previous component. */
#if defined(SIMRT_CORO_FIBERS)
    {
        simrt_coro *os = simrt_coro_from_os_fiber();
        simrt_coro_current_ptr = os != NULL ? os : from;
    }
#else
    simrt_coro_current_ptr = from;
#endif
}

int simrt_coro_is_done(const simrt_coro *coro) {
    return coro == NULL ? 1 : coro->done;
}

void simrt_coro_destroy(simrt_coro *coro) {
    if (coro == NULL || coro == simrt_coro_main_ptr) {
        return;
    }
    simrt_coro_unregister(coro);
    simrt_coro_stack_free(coro);
    free(coro);
}

/* ------------------------------------------------------------------ */
/* Garbage collection support                                          */
/* ------------------------------------------------------------------ */

void simrt_coro_gc_visit_parked_root_heads(simrt_coro_root_head_visitor visit, void *user) {
    simrt_coro *current;
    simrt_coro *it;
    if (visit == NULL) {
        return;
    }
#if defined(SIMRT_CORO_FIBERS)
    current = simrt_coro_from_os_fiber();
    if (current == NULL) {
        current = simrt_coro_current_ptr;
    }
#else
    current = simrt_coro_current_ptr;
#endif
    for (it = simrt_coro_all_ptr; it != NULL; it = it->next_all) {
        if (it == current) {
            continue;
        }
        visit(it->gc_roots, user);
    }
}
