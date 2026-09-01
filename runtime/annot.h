/* Lightweight C ABI annotations for the native runtime.
 *
 * These do not change layout or calling convention: `simrt_gc_ptr` is still
 * `void *`. They exist so a reviewer (and clang) can see which pointers are
 * managed-heap payloads versus host `malloc`/`mmap`/`FILE` memory.
 */

#ifndef SIMRT_ANNOT_H
#define SIMRT_ANNOT_H

#if defined(__clang__) || defined(__GNUC__)
#define SIMRT_NONNULL __attribute__((nonnull))
#define SIMRT_MUST_USE __attribute__((warn_unused_result))
#define SIMRT_CLEANUP(fn) __attribute__((cleanup(fn)))
#else
#define SIMRT_NONNULL
#define SIMRT_MUST_USE
#define SIMRT_CLEANUP(fn)
#endif

#if defined(__clang__)
#define SIMRT_NULLABLE _Nullable
#define SIMRT_NN _Nonnull
#else
#define SIMRT_NULLABLE
#define SIMRT_NN
#endif

/* Payload pointer into the managed heap (object, array, text frame/object).
 * May be an interior FieldAddr. Never pass to free(). */
typedef void *simrt_gc_ptr;

/* Host malloc/mmap/FILE-backed pointer. Never stored in a GC root slot as a
 * Simula ref (except as an opaque word the tracer skips). */
typedef void *simrt_host_ptr;

#endif /* SIMRT_ANNOT_H */
