/* Overflow-checked host allocation.
 *
 * The managed heap is `simrt_gc_alloc`. These helpers are only for
 * sequencing metadata, coroutine structs, BASICIO paths, SQS notices, and
 * scratch buffers. A NULL return is overflow or genuine OOM; callers diagnose.
 */

#ifndef SIMRT_HOST_H
#define SIMRT_HOST_H

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

static inline int simrt_host_size_add(size_t a, size_t b, size_t *out) {
    if (a > SIZE_MAX - b) {
        return 0;
    }
    *out = a + b;
    return 1;
}

static inline int simrt_host_size_mul(size_t a, size_t b, size_t *out) {
    if (b != 0 && a > SIZE_MAX / b) {
        return 0;
    }
    *out = a * b;
    return 1;
}

/* `malloc(0)` is implementation-defined; ASan also dislikes it. */
static inline void *simrt_host_malloc(size_t n) {
    if (n == 0) {
        n = 1;
    }
    return malloc(n);
}

static inline void *simrt_host_malloc_sum(size_t a, size_t b) {
    size_t n;
    if (!simrt_host_size_add(a, b, &n)) {
        return NULL;
    }
    return simrt_host_malloc(n);
}

static inline void *simrt_host_calloc(size_t count, size_t elem) {
    size_t bytes;
    void *pointer;
    if (!simrt_host_size_mul(count, elem, &bytes)) {
        return NULL;
    }
    pointer = simrt_host_malloc(bytes);
    if (pointer != NULL) {
        memset(pointer, 0, bytes);
    }
    return pointer;
}

static inline void *simrt_host_realloc_n(void *pointer, size_t count, size_t elem) {
    size_t bytes;
    if (!simrt_host_size_mul(count, elem, &bytes)) {
        return NULL;
    }
    if (bytes == 0) {
        bytes = 1;
    }
    return realloc(pointer, bytes);
}

#endif /* SIMRT_HOST_H */
