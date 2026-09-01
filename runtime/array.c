#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "internal.h"

/* N-D integer array support.
 *
 * Descriptors live on the managed heap (`simrt_gc_alloc`), so an array
 * that outlives its declaring block (CBL §9.1) survives exactly as long as
 * something still references it. Layout:
 *
 *   { ndims, bounds[2*ndims], data[element_count] }
 *
 * where `bounds` stores `(low, high)` pairs in row-major declaration order and
 * `element_count` is the product of per-dimension sizes. `low > high` on any
 * dimension yields zero elements; allocation still succeeds and any subsequent
 * load/store fails the bounds check, matching the interpreter's
 * `check_array_bounds`. */
static int64_t simrt_array_dim_size(int64_t low, int64_t high) {
    return high >= low ? (high - low + 1) : 0;
}

/* Checked product of dimension sizes. Rejects when the dense element count
 * would overflow int64_t; `simrt_array_checked_total` then rejects when
 * header + count*elem_size would overflow size_t. Without these an extent such
 * as (1:2**40, 1:2**40) wraps to a short allocation that every later bounds
 * check accepts. */
int64_t simrt_array_checked_count(int64_t ndims, const int64_t *bounds) {
    int64_t count;
    if (!simrt_array_count_ok(ndims, bounds, &count)) {
        simrt_error("array extent overflow");
    }
    return count;
}

size_t simrt_array_checked_total(int64_t count, size_t header, size_t elem_size) {
    size_t total;
    if (!simrt_array_total_ok(count, header, elem_size, &total)) {
        simrt_error("array extent overflow");
    }
    return total;
}

size_t simrt_array_checked_header(int64_t ndims) {
    size_t header;
    if (!simrt_array_header_ok(ndims, &header)) {
        simrt_error("array extent overflow");
    }
    return header;
}

static int64_t simrt_array_dim_low(const SimrtArrayI64 *array, int64_t dim) {
    return array->bounds_and_data[(size_t)dim * 2];
}

static int64_t simrt_array_dim_high(const SimrtArrayI64 *array, int64_t dim) {
    return array->bounds_and_data[(size_t)dim * 2 + 1];
}

static int64_t *simrt_array_data(SimrtArrayI64 *array) {
    return array->bounds_and_data + (size_t)array->ndims * 2;
}

static void simrt_array_bounds_fail_dim(int64_t index, int64_t low, int64_t high) {
    fprintf(
        stderr,
        "sim: subscript %lld out of bounds [%lld:%lld]\n",
        (long long)index,
        (long long)low,
        (long long)high
    );
    abort();
}

static void simrt_array_bounds_fail_empty(void) {
    fprintf(stderr, "sim: array access on empty array dimension\n");
    abort();
}

int64_t simrt_array_linear_index(const SimrtArrayI64 *array, const int64_t *indices) {
    int64_t linear = 0;
    int64_t stride = 1;
    for (int64_t dim = array->ndims - 1; dim >= 0; dim--) {
        int64_t low = simrt_array_dim_low(array, dim);
        int64_t high = simrt_array_dim_high(array, dim);
        if (low > high) {
            simrt_array_bounds_fail_empty();
        }
        int64_t index = indices[dim];
        if (index < low || index > high) {
            simrt_array_bounds_fail_dim(index, low, high);
        }
        linear += (index - low) * stride;
        stride *= simrt_array_dim_size(low, high);
    }
    return linear;
}

void *simrt_array_alloc_i64(int64_t ndims, const int64_t *bounds) {
    if (ndims < 1) {
        fprintf(stderr, "sim: array allocation requires at least one dimension\n");
        abort();
    }
    int64_t count = simrt_array_checked_count(ndims, bounds);
    size_t header = simrt_array_checked_header(ndims);
    size_t total = simrt_array_checked_total(count, header, sizeof(int64_t));
    SimrtArrayI64 *array =
        (SimrtArrayI64 *)simrt_gc_alloc(SIMRT_GC_ARRAY_I64, (int64_t)total);
    if (array == NULL) {
        fprintf(stderr, "sim: out of memory allocating %lld-dimensional array\n", (long long)ndims);
        abort();
    }
    array->ndims = ndims;
    memcpy(array->bounds_and_data, bounds, header - sizeof(int64_t));
    return array;
}

int64_t simrt_array_element_count(const SimrtArrayI64 *array) {
    return simrt_array_checked_count(array->ndims, array->bounds_and_data);
}

/* Call-by-value array transmission (§4.6.2): deep-copy descriptor + elements. */
void *simrt_array_copy_i64(simrt_gc_ptr array_ptr) {
    const SimrtArrayI64 *src = (const SimrtArrayI64 *)array_ptr;
    if (src == NULL) {
        fprintf(stderr, "sim: copy of null integer array\n");
        abort();
    }
    int64_t count = simrt_array_element_count(src);
    size_t header = simrt_array_checked_header(src->ndims);
    size_t total = simrt_array_checked_total(count, header, sizeof(int64_t));
    SimrtArrayI64 *dst =
        (SimrtArrayI64 *)simrt_gc_alloc(SIMRT_GC_ARRAY_I64, (int64_t)total);
    if (dst == NULL) {
        fprintf(stderr, "sim: out of memory copying integer array\n");
        abort();
    }
    memcpy(dst, src, total);
    return dst;
}

int64_t simrt_array_load_i64(simrt_gc_ptr array_ptr, int64_t ndims, const int64_t *indices) {
    const SimrtArrayI64 *array = (const SimrtArrayI64 *)array_ptr;
    if (array->ndims != ndims) {
        fprintf(
            stderr,
            "sim: array expects %lld subscripts, found %lld\n",
            (long long)array->ndims,
            (long long)ndims
        );
        abort();
    }
    int64_t linear = simrt_array_linear_index(array, indices);
    const int64_t *data = array->bounds_and_data + (size_t)array->ndims * 2;
    return data[linear];
}

void simrt_array_store_i64(simrt_gc_ptr array_ptr, int64_t ndims, const int64_t *indices, int64_t value) {
    SimrtArrayI64 *array = (SimrtArrayI64 *)array_ptr;
    if (array->ndims != ndims) {
        fprintf(
            stderr,
            "sim: array expects %lld subscripts, found %lld\n",
            (long long)array->ndims,
            (long long)ndims
        );
        abort();
    }
    int64_t linear = simrt_array_linear_index(array, indices);
    simrt_array_data(array)[linear] = value;
}

/* Real (f64) arrays — same descriptor header as i64; payload is double. */
static double *simrt_array_data_f64(SimrtArrayI64 *array) {
    return (double *)(array->bounds_and_data + (size_t)array->ndims * 2);
}

void *simrt_array_alloc_f64(int64_t ndims, const int64_t *bounds) {
    if (ndims < 1) {
        fprintf(stderr, "sim: array allocation requires at least one dimension\n");
        abort();
    }
    int64_t count = simrt_array_checked_count(ndims, bounds);
    size_t header = simrt_array_checked_header(ndims);
    size_t total = simrt_array_checked_total(count, header, sizeof(double));
    SimrtArrayI64 *array =
        (SimrtArrayI64 *)simrt_gc_alloc(SIMRT_GC_ARRAY_F64, (int64_t)total);
    if (array == NULL) {
        fprintf(stderr, "sim: out of memory allocating %lld-dimensional real array\n", (long long)ndims);
        abort();
    }
    array->ndims = ndims;
    memcpy(array->bounds_and_data, bounds, header - sizeof(int64_t));
    return array;
}

void *simrt_array_copy_f64(simrt_gc_ptr array_ptr) {
    const SimrtArrayI64 *src = (const SimrtArrayI64 *)array_ptr;
    if (src == NULL) {
        fprintf(stderr, "sim: copy of null real array\n");
        abort();
    }
    int64_t count = simrt_array_element_count(src);
    size_t header = simrt_array_checked_header(src->ndims);
    size_t total = simrt_array_checked_total(count, header, sizeof(double));
    SimrtArrayI64 *dst =
        (SimrtArrayI64 *)simrt_gc_alloc(SIMRT_GC_ARRAY_F64, (int64_t)total);
    if (dst == NULL) {
        fprintf(stderr, "sim: out of memory copying real array\n");
        abort();
    }
    memcpy(dst, src, total);
    return dst;
}

double simrt_array_load_f64(simrt_gc_ptr array_ptr, int64_t ndims, const int64_t *indices) {
    SimrtArrayI64 *array = (SimrtArrayI64 *)array_ptr;
    if (array->ndims != ndims) {
        fprintf(
            stderr,
            "sim: array expects %lld subscripts, found %lld\n",
            (long long)array->ndims,
            (long long)ndims
        );
        abort();
    }
    int64_t linear = simrt_array_linear_index(array, indices);
    return simrt_array_data_f64(array)[linear];
}

void simrt_array_store_f64(simrt_gc_ptr array_ptr, int64_t ndims, const int64_t *indices, double value) {
    SimrtArrayI64 *array = (SimrtArrayI64 *)array_ptr;
    if (array->ndims != ndims) {
        fprintf(
            stderr,
            "sim: array expects %lld subscripts, found %lld\n",
            (long long)array->ndims,
            (long long)ndims
        );
        abort();
    }
    int64_t linear = simrt_array_linear_index(array, indices);
    simrt_array_data_f64(array)[linear] = value;
}

int64_t simrt_array_lowerbound(simrt_gc_ptr array_ptr, int64_t dim_1based) {
    const SimrtArrayI64 *array = (const SimrtArrayI64 *)array_ptr;
    if (array == NULL || dim_1based < 1 || dim_1based > array->ndims) {
        simrt_error("array dimension out of range");
    }
    return simrt_array_dim_low(array, dim_1based - 1);
}

int64_t simrt_array_upperbound(simrt_gc_ptr array_ptr, int64_t dim_1based) {
    const SimrtArrayI64 *array = (const SimrtArrayI64 *)array_ptr;
    if (array == NULL || dim_1based < 1 || dim_1based > array->ndims) {
        simrt_error("array dimension out of range");
    }
    return simrt_array_dim_high(array, dim_1based - 1);
}

static double *simrt_array_f64_slice(const void *array_ptr, int64_t *out_count, int64_t *out_lo) {
    const SimrtArrayI64 *array = (const SimrtArrayI64 *)array_ptr;
    if (array == NULL || array->ndims != 1) {
        simrt_error("real array argument must be one-dimensional");
    }
    *out_lo = simrt_array_dim_low(array, 0);
    *out_count = simrt_array_element_count(array);
    return (double *)(array->bounds_and_data + (size_t)array->ndims * 2);
}

int64_t simrt_discrete(simrt_gc_ptr array_ptr, int64_t *stream) {
    int64_t count;
    int64_t lo;
    const double *data;
    double u;
    int64_t i;
    if (stream == NULL) {
        simrt_error("discrete: null random stream");
    }
    data = simrt_array_f64_slice(array_ptr, &count, &lo);
    if (count < 1) {
        simrt_error("discrete: empty distribution");
    }
    u = simrt_basic_draw(stream);
    for (i = 0; i < count; i++) {
        if (data[i] > u) {
            return lo + i; /* 1-based dense index i+1 → lo+(i+1)-1 */
        }
    }
    return lo + count; /* past upper bound */
}

int64_t simrt_histd(simrt_gc_ptr array_ptr, int64_t *stream) {
    int64_t count;
    int64_t lo;
    const double *data;
    double total;
    double target;
    double cumulative;
    int64_t i;
    if (stream == NULL) {
        simrt_error("histd: null random stream");
    }
    data = simrt_array_f64_slice(array_ptr, &count, &lo);
    if (count < 1) {
        simrt_error("histd: empty histogram");
    }
    total = 0.0;
    for (i = 0; i < count; i++) {
        total += data[i];
    }
    if (total <= 0.0) {
        simrt_error("histd: non-positive total frequency");
    }
    target = simrt_basic_draw(stream) * total;
    cumulative = 0.0;
    for (i = 0; i < count; i++) {
        cumulative += data[i];
        if (target < cumulative) {
            return lo + i;
        }
    }
    return lo + count - 1;
}

double simrt_linear(simrt_gc_ptr a_ptr, simrt_gc_ptr b_ptr, int64_t *stream) {
    int64_t a_count;
    int64_t b_count;
    int64_t a_lo;
    int64_t b_lo;
    const double *a;
    const double *b;
    double u;
    int64_t i;
    double d;
    if (stream == NULL) {
        simrt_error("linear: null random stream");
    }
    a = simrt_array_f64_slice(a_ptr, &a_count, &a_lo);
    b = simrt_array_f64_slice(b_ptr, &b_count, &b_lo);
    (void)a_lo;
    (void)b_lo;
    if (a_count != b_count || a_count < 1) {
        simrt_error("linear: invalid table");
    }
    u = simrt_basic_draw(stream);
    for (i = 1; i < a_count; i++) {
        if (u <= a[i]) {
            d = a[i] - a[i - 1];
            if (d == 0.0) {
                return b[i - 1];
            }
            return b[i - 1] + (b[i] - b[i - 1]) * (u - a[i - 1]) / d;
        }
    }
    return b[a_count - 1];
}

int64_t simrt_histo(simrt_gc_ptr a_ptr, simrt_gc_ptr b_ptr, double c, double d) {
    int64_t a_count;
    int64_t b_count;
    int64_t a_lo;
    int64_t b_lo;
    double *a;
    const double *b;
    int64_t i;
    int64_t index;
    a = simrt_array_f64_slice(a_ptr, &a_count, &a_lo);
    b = simrt_array_f64_slice(b_ptr, &b_count, &b_lo);
    (void)a_lo;
    (void)b_lo;
    if (a_count != b_count + 1) {
        simrt_error("histo: A length must be one greater than B");
    }
    index = b_count;
    for (i = 0; i < b_count; i++) {
        if (c <= b[i]) {
            index = i;
            break;
        }
    }
    a[index] += d;
    return 0;
}

