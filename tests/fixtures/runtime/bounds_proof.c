/* Concrete tests for the overflow predicates in `runtime/safety.c`.
 * Linked against `safety.c` only — no heap, no
 * `simrt_error`. Optional CBMC entry points live behind `__CPROVER__`. */

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

#include "../../../runtime/safety.h"

static int failures;

static void report(int ok, const char *name, const char *detail) {
    if (ok) {
        printf("PASS %s\n", name);
    } else {
        printf("FAIL %s: %s\n", name, detail);
        failures++;
    }
}

static void test_array_count(void) {
    int64_t out = -1;
    int64_t one[2] = {1, 10};
    int64_t empty[2] = {5, 4};
    int64_t two[4] = {1, 2, 1, 3};
    int64_t huge[4] = {1, 1LL << 40, 1, 1LL << 40};
    int64_t too_wide[2] = {0, INT64_MAX};
    int64_t full_range[2] = {INT64_MIN, INT64_MAX};

    report(
        simrt_array_count_ok(1, one, &out) && out == 10,
        "a one-dimensional extent is the inclusive length",
        "expected 10"
    );
    report(
        simrt_array_count_ok(1, empty, &out) && out == 0,
        "empty dimension is a zero-length array",
        "expected 0"
    );
    report(
        simrt_array_count_ok(2, two, &out) && out == 6,
        "row-major product of two extents",
        "expected 6"
    );
    report(
        !simrt_array_count_ok(2, huge, &out),
        "product of extents is rejected on int64 overflow",
        "2**40 * 2**40 should fail"
    );
    report(
        !simrt_array_count_ok(1, too_wide, &out),
        "a single dimension that does not fit in int64 is rejected",
        "[0:INT64_MAX] is INT64_MAX+1 values"
    );
    report(
        !simrt_array_count_ok(1, full_range, &out),
        "INT64_MIN..INT64_MAX is rejected",
        "span does not fit in int64_t"
    );
    report(
        !simrt_array_count_ok(1, NULL, &out),
        "a missing bounds vector is rejected",
        "NULL bounds"
    );
    report(
        !simrt_array_count_ok(-1, one, &out),
        "a negative ndims is rejected",
        "ndims < 0"
    );
    report(
        simrt_array_count_ok(0, NULL, &out) && out == 1,
        "zero dimensions is a unit product",
        "expected 1"
    );
}

static void test_array_total(void) {
    size_t out = 0;
    report(
        simrt_array_total_ok(3, 8, 8, &out) && out == 32,
        "header plus payload is the descriptor size",
        "expected 32"
    );
    report(
        simrt_array_total_ok(0, 16, 8, &out) && out == 16,
        "a zero-length array still has a header",
        "expected 16"
    );
    report(
        !simrt_array_total_ok(-1, 8, 8, &out),
        "a negative element count is rejected",
        "count < 0"
    );
    report(
        !simrt_array_total_ok(1, 8, 0, &out),
        "a zero element size is rejected",
        "elem_size == 0"
    );
    report(
        !simrt_array_total_ok((int64_t)((SIZE_MAX - 16) / 8 + 1), 16, 8, &out),
        "header plus payload is rejected on size_t overflow",
        "count just past (SIZE_MAX-header)/elem"
    );
}

static void test_array_header(void) {
    size_t out = 0;
    int64_t too_many = (int64_t)((SIZE_MAX - sizeof(int64_t)) / (2u * sizeof(int64_t)) + 1u);
    report(
        simrt_array_header_ok(1, &out) && out == sizeof(int64_t) + 2u * sizeof(int64_t),
        "a one-dimensional header is ndims plus two bound words",
        "expected 24 on LP64"
    );
    report(
        simrt_array_header_ok(3, &out) && out == sizeof(int64_t) + 6u * sizeof(int64_t),
        "a three-dimensional header scales with bound pairs",
        "expected 56 on LP64"
    );
    report(
        !simrt_array_header_ok(0, &out) && !simrt_array_header_ok(-1, &out),
        "ndims below 1 is rejected for a header",
        "ndims < 1"
    );
    if (too_many > 0) {
        report(
            !simrt_array_header_ok(too_many, &out),
            "a bounds memcpy that would wrap size_t is rejected",
            "ndims just past (SIZE_MAX-8)/16"
        );
    }
}

static void test_object_offset(void) {
    report(
        simrt_object_offset_ok(24, 0) && simrt_object_offset_ok(24, 8)
            && simrt_object_offset_ok(24, 16),
        "aligned field offsets inside a 24-byte payload are accepted",
        "0, 8, 16"
    );
    report(
        !simrt_object_offset_ok(24, 17) && !simrt_object_offset_ok(24, 24)
            && !simrt_object_offset_ok(24, -1) && !simrt_object_offset_ok(4, 0),
        "object field offsets are rejected outside the payload",
        "17, 24, -1, tiny object"
    );
}

int main(void) {
    test_array_count();
    test_array_total();
    test_array_header();
    test_object_offset();
    if (failures != 0) {
        printf("DONE failures=%d\n", failures);
        return 1;
    }
    printf("DONE\n");
    return 0;
}

#ifdef __CPROVER__
void simrt_cbmc_array_count(void) {
    int64_t ndims;
    int64_t bounds[8];
    int64_t out;
    unsigned i;
    __CPROVER_assume(ndims >= 0 && ndims <= 4);
    if (simrt_array_count_ok(ndims, bounds, &out)) {
        __CPROVER_assert(out >= 0, "count is non-negative");
        if (ndims == 0) {
            __CPROVER_assert(out == 1, "empty product is 1");
        }
        for (i = 0; i < (unsigned)ndims; i++) {
            (void)bounds[i];
        }
    }
}

void simrt_cbmc_array_total(void) {
    int64_t count;
    size_t header;
    size_t elem_size;
    size_t out;
    __CPROVER_assume(elem_size == 1 || elem_size == 8);
    __CPROVER_assume(header <= 256);
    __CPROVER_assume(count >= -1 && count <= 1024);
    if (simrt_array_total_ok(count, header, elem_size, &out)) {
        __CPROVER_assert(count >= 0, "accepted count is non-negative");
        __CPROVER_assert(out >= header, "total covers the header");
        __CPROVER_assert(out == header + (size_t)count * elem_size, "total is exact");
    }
}

void simrt_cbmc_array_header(void) {
    int64_t ndims;
    size_t out;
    __CPROVER_assume(ndims >= -1 && ndims <= 8);
    if (simrt_array_header_ok(ndims, &out)) {
        __CPROVER_assert(ndims >= 1, "accepted ndims is at least 1");
        __CPROVER_assert(
            out == sizeof(int64_t) + (size_t)ndims * 2u * sizeof(int64_t),
            "header is exact"
        );
    }
}

void simrt_cbmc_object_offset(void) {
    int64_t obj_size;
    int64_t offset;
    __CPROVER_assume(obj_size >= 0 && obj_size <= 256);
    __CPROVER_assume(offset >= -8 && offset <= 256);
    if (simrt_object_offset_ok(obj_size, offset)) {
        __CPROVER_assert(offset >= 0, "accepted offset is non-negative");
        __CPROVER_assert(obj_size >= (int64_t)sizeof(int64_t), "payload holds a word");
        __CPROVER_assert(offset <= obj_size - (int64_t)sizeof(int64_t), "word fits");
    }
}
#endif
