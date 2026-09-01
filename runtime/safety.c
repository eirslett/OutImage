/* Overflow predicates used by array/object wrappers and by the C proof
 * fixture. No heap, no I/O — this file is the CBMC-sized surface. */

#include "safety.h"

#include <limits.h>

static int simrt_array_dim_size_ok(int64_t low, int64_t high, int64_t *out) {
    if (out == NULL) {
        return 0;
    }
    if (low > high) {
        *out = 0;
        return 1;
    }
#if defined(__SIZEOF_INT128__)
    {
        __int128 span = (__int128)high - (__int128)low + 1;
        if (span > INT64_MAX) {
            return 0;
        }
        *out = (int64_t)span;
        return 1;
    }
#else
    /* `high - low + 1` overflows int64 iff the mathematical span > INT64_MAX.
     * When `low <= 0`, `INT64_MAX + low` is well-defined (`low >= INT64_MIN`)
     * and the overflow test is `high >= INT64_MAX + low`. When `low > 0`,
     * `high - low + 1 <= INT64_MAX`. */
    if (low <= 0 && high >= INT64_MAX + low) {
        return 0;
    }
    *out = high - low + 1;
    return 1;
#endif
}

int simrt_array_count_ok(int64_t ndims, const int64_t *bounds, int64_t *out) {
    int64_t count = 1;
    int64_t dim;
    if (out == NULL || ndims < 0) {
        return 0;
    }
    if (ndims > 0 && bounds == NULL) {
        return 0;
    }
    for (dim = 0; dim < ndims; dim++) {
        int64_t low = bounds[(size_t)dim * 2];
        int64_t high = bounds[(size_t)dim * 2 + 1];
        int64_t size;
        if (!simrt_array_dim_size_ok(low, high, &size)) {
            return 0;
        }
        if (size == 0) {
            *out = 0;
            return 1;
        }
        if (count > INT64_MAX / size) {
            return 0;
        }
        count *= size;
    }
    *out = count;
    return 1;
}

int simrt_array_header_ok(int64_t ndims, size_t *out) {
    size_t bounds;
    if (out == NULL || ndims < 1) {
        return 0;
    }
    if ((uint64_t)ndims > (SIZE_MAX - sizeof(int64_t)) / (2u * sizeof(int64_t))) {
        return 0;
    }
    bounds = (size_t)ndims * 2u * sizeof(int64_t);
    *out = sizeof(int64_t) + bounds;
    return 1;
}

int simrt_array_total_ok(int64_t count, size_t header, size_t elem_size, size_t *out) {
    size_t total;
    if (out == NULL || count < 0 || elem_size == 0) {
        return 0;
    }
    if ((size_t)count > (SIZE_MAX - header) / elem_size) {
        return 0;
    }
    total = header + (size_t)count * elem_size;
    if (total > (size_t)INT64_MAX) {
        return 0;
    }
    *out = total;
    return 1;
}

int simrt_object_offset_ok(int64_t obj_size, int64_t offset) {
    return offset >= 0 && obj_size >= (int64_t)sizeof(int64_t)
        && offset <= obj_size - (int64_t)sizeof(int64_t);
}
