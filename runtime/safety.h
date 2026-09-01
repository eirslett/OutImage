/* Pure overflow / bounds predicates.
 *
 * These functions never allocate, print, or call `simrt_error`. They
 * return 0 on overflow or a malformed argument so wrappers and a CBMC
 * harness can share one implementation.
 */

#ifndef SIMRT_SAFETY_H
#define SIMRT_SAFETY_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Product of per-dimension sizes `(high - low + 1)`. Empty dimensions
 * (`low > high`) yield count 0. Rejects a product that would not fit in
 * `int64_t`, a negative `ndims`, or a missing `bounds`/`out`. */
int simrt_array_count_ok(int64_t ndims, const int64_t *bounds, int64_t *out);

/* `sizeof(int64_t) + ndims * 2 * sizeof(int64_t)` — the descriptor prefix
 * before the dense payload. Rejects `ndims < 1` or a size that wraps
 * `size_t`, so a later `memcpy` of the bounds vector cannot be a wrapped
 * short copy into a wrapped short allocation. */
int simrt_array_header_ok(int64_t ndims, size_t *out);

/* `header + count * elem_size` as `size_t`. Rejects negative `count`,
 * zero `elem_size`, a sum that wraps `size_t`, or a total that does not
 * fit in `int64_t` (`gc_alloc` takes a signed size). */
int simrt_array_total_ok(int64_t count, size_t header, size_t elem_size, size_t *out);

/* Object field load/store: `offset` is a byte offset of an `int64_t` word
 * inside a payload of `obj_size` bytes (the hidden size at `obj - 8`). */
int simrt_object_offset_ok(int64_t obj_size, int64_t offset);

#ifdef __cplusplus
}
#endif

#endif /* SIMRT_SAFETY_H */
