/* Shared types and internal declarations for the split native runtime
 * (`array.c`, `text.c`, `object.c`, `env.c`, `io.c`, `sim.c`, `runtime.c`).
 * Not part of the exported ABI — generated code includes nothing from here.
 */

#ifndef SIMRT_RUNTIME_INTERNAL_H
#define SIMRT_RUNTIME_INTERNAL_H

#include "runtime.h"
#include "host.h"
#include "safety.h"

#ifdef __cplusplus
extern "C" {
#endif

/* `{ ndims, bounds[2*ndims], data[] }`. `__counted_by(ndims)` would claim
 * `ndims` FAM elements; the live length is `2*ndims` bounds words plus the
 * dense payload. A true `nwords` field would be an ABI break. Do not tag. */
typedef struct {
    int64_t ndims;
    int64_t bounds_and_data[];
} SimrtArrayI64;

typedef struct {
    unsigned char *main;
    size_t main_len;
    int constant;
} SimrtTextObject;

struct SimrtTextFrame {
    SimrtTextObject *obj;
    int64_t start;
    int64_t length;
    int64_t pos;
};

/* ENVIRONMENT current decimal mark / lowten. Owned by `env.c`. */
extern char g_decimal_mark;
extern char g_lowten;

/* Unit interval draw used by ENVIRONMENT distributions and array histo/linear. */
double simrt_basic_draw(int64_t *stream);

enum { SIMRT_NUMERIC_FIELD_MAX = 256 };

/* Array helpers used by text arrays (`text.c`). */
int64_t simrt_array_checked_count(int64_t ndims, const int64_t *bounds);
size_t simrt_array_checked_total(int64_t count, size_t header, size_t elem_size);
size_t simrt_array_checked_header(int64_t ndims);
int64_t simrt_array_element_count(const SimrtArrayI64 *array);
int64_t simrt_array_linear_index(const SimrtArrayI64 *array, const int64_t *indices);

/* Text internals used by BASICIO / file helpers (`io.c`). */
int simrt_text_frame_is_notext(const SimrtTextFrame *frame);
void simrt_text_oom(void);
void *simrt_text_host_alloc(size_t n);
size_t simrt_text_content_length(const SimrtTextFrame *frame);
const unsigned char *simrt_text_content_ptr(const SimrtTextFrame *frame);
char *simrt_text_to_cstr(const SimrtTextFrame *frame);
void simrt_text_group_integer_part(const char *integer, char *out, size_t out_sz);
void simrt_text_group_fractional_part(const char *fraction, char *out, size_t out_sz);
int simrt_text_parse_integer_item(
    const unsigned char *content, size_t len, int64_t *value_out, size_t *consumed_out
);
int simrt_text_parse_real_item(
    const unsigned char *content, size_t len, double *value_out, size_t *consumed_out
);

size_t simrt_clamp_len(int len, size_t cap);
size_t simrt_format_fix(char *buf, size_t cap, double value, int64_t n);
size_t simrt_format_real_ex(char *buf, size_t cap, double value, int64_t n, int exp_digits);
size_t simrt_format_frac(char *buf, size_t cap, int64_t value, int64_t n);
void simrt_pad_numeric_field(
    unsigned char *out, size_t *out_len, const char *item, size_t item_len, int64_t w
);

/* Precise C-runtime roots, one table per module so glue does not extern
 * BASICIO / SQS storage. */
void simrt_basicio_gc_visit_roots(simrt_gc_mark_fn mark);
void simrt_sim_gc_visit_roots(simrt_gc_mark_fn mark);

#ifdef __cplusplus
}
#endif

#endif /* SIMRT_RUNTIME_INTERNAL_H */
