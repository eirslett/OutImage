#include <ctype.h>
#include <errno.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "internal.h"

/* Text support.
 *
 * A text value is a pointer to a `SimrtTextFrame` describing a subframe of
 * a shared `SimrtTextObject` buffer, or notext (`obj == NULL` or
 * `length == 0`).
 *
 * Frames and text objects live on the managed heap and are swept normally.
 * Character storage is folded
 * into the same `TEXT_OBJECT` block as the header, so `simrt_text_content_ptr`
 * hands out an *interior* pointer the collector can attribute
 * via `simrt_gc_block_at` from a precise root slot. A separate `malloc` for characters would leave
 * those pointers unattributable and force the old `UNSWEPT` retention.
 * Semantics for notext, constant literals, copy, concat, and padded
 * assignment match the interpreter's `runtime/text.rs`. */
int simrt_text_frame_is_notext(const SimrtTextFrame *frame) {
    return frame == NULL || frame->obj == NULL || frame->length == 0;
}

void simrt_text_oom(void) {
    fprintf(stderr, "sim: out of memory allocating text\n");
    abort();
}

static size_t simrt_text_checked_size(int64_t n) {
    if (n < 0 || (uint64_t)n > (uint64_t)SIZE_MAX) {
        simrt_text_oom();
    }
    return (size_t)n;
}

void *simrt_text_host_alloc(size_t n) {
    void *pointer = simrt_host_malloc(n);
    if (pointer == NULL) {
        simrt_text_oom();
    }
    return pointer;
}

static SimrtTextFrame *simrt_text_alloc_frame(void) {
    SimrtTextFrame *frame = (SimrtTextFrame *)simrt_gc_alloc(
        SIMRT_GC_TEXT_FRAME, (int64_t)sizeof(SimrtTextFrame)
    );
    if (frame == NULL) {
        simrt_text_oom();
    }
    /* Default indices match NOTEXT (START=1, POS=1) until a subframe is set. */
    frame->start = 1;
    frame->pos = 1;
    return frame;
}

static SimrtTextObject *simrt_text_alloc_object(size_t len, int constant) {
    /* One managed block: header fields followed immediately by the character
     * payload. `main` points into that same block so a content pointer in a
     * precise root slot pins the object via `block_at`. */
    size_t payload;
    SimrtTextObject *object;

    if (len > SIZE_MAX - sizeof(SimrtTextObject)) {
        simrt_text_oom();
    }
    payload = sizeof(SimrtTextObject) + len;
    if (payload > (size_t)INT64_MAX) {
        simrt_text_oom();
    }
    object = (SimrtTextObject *)simrt_gc_alloc(
        SIMRT_GC_TEXT_OBJECT, (int64_t)payload
    );
    if (object == NULL) {
        simrt_text_oom();
    }
    if (len > 0) {
        object->main = (unsigned char *)(object + 1);
        object->main_len = len;
    }
    object->constant = constant;
    return object;
}

static void simrt_text_write_object(SimrtTextObject *object, int64_t start, const unsigned char *content, size_t len) {
    if (object == NULL || object->main == NULL || len == 0) {
        return;
    }
    size_t offset;
    if (start < 1) {
        fprintf(stderr, "sim: internal text write out of bounds\n");
        abort();
    }
    offset = (size_t)(start - 1);
    if (len > object->main_len || offset > object->main_len - len) {
        fprintf(stderr, "sim: internal text write out of bounds\n");
        abort();
    }
    memcpy(object->main + offset, content, len);
}

size_t simrt_text_content_length(const SimrtTextFrame *frame) {
    if (simrt_text_frame_is_notext(frame)) {
        return 0;
    }
    return (size_t)frame->length;
}

const unsigned char *simrt_text_content_ptr(const SimrtTextFrame *frame) {
    if (simrt_text_frame_is_notext(frame)) {
        return NULL;
    }
    return frame->obj->main + (size_t)(frame->start - 1);
}

SimrtTextFrame *simrt_text_notext(void) {
    return simrt_text_alloc_frame();
}

/* Text arrays: same descriptor header/bounds layout as `SimrtArrayI64`, but
 * elements are `SimrtTextFrame *` slots. Slots are pre-filled with notext. */
static SimrtTextFrame **simrt_array_text_data(SimrtArrayI64 *array) {
    return (SimrtTextFrame **)(array->bounds_and_data + (size_t)array->ndims * 2);
}

void *simrt_array_alloc_text(int64_t ndims, const int64_t *bounds) {
    if (ndims < 1) {
        fprintf(stderr, "sim: array allocation requires at least one dimension\n");
        abort();
    }
    int64_t count = simrt_array_checked_count(ndims, bounds);
    size_t header = simrt_array_checked_header(ndims);
    size_t total = simrt_array_checked_total(count, header, sizeof(SimrtTextFrame *));
    SimrtArrayI64 *array;

    SIMRT_GC_DEFER_BEGIN();
    array = (SimrtArrayI64 *)simrt_gc_alloc(SIMRT_GC_ARRAY_TEXT, (int64_t)total);
    if (array == NULL) {
        SIMRT_GC_DEFER_LEAVE();
        fprintf(stderr, "sim: out of memory allocating %lld-dimensional text array\n", (long long)ndims);
        abort();
    }
    /* `ndims` decides where the tracer starts reading frame pointers. */
    array->ndims = ndims;
    memcpy(array->bounds_and_data, bounds, header - sizeof(int64_t));
    SimrtTextFrame **data = simrt_array_text_data(array);
    for (int64_t i = 0; i < count; i++) {
        data[i] = simrt_text_notext();
    }
    SIMRT_GC_DEFER_LEAVE();
    return array;
}

SimrtTextFrame *simrt_text_copy(SimrtTextFrame *src);

void *simrt_array_copy_text(simrt_gc_ptr array_ptr) {
    const SimrtArrayI64 *src = (const SimrtArrayI64 *)array_ptr;
    if (src == NULL) {
        fprintf(stderr, "sim: copy of null text array\n");
        abort();
    }
    int64_t count = simrt_array_element_count(src);
    size_t header = simrt_array_checked_header(src->ndims);
    size_t total = simrt_array_checked_total(count, header, sizeof(SimrtTextFrame *));
    SimrtArrayI64 *dst;

    SIMRT_GC_DEFER_BEGIN();
    dst = (SimrtArrayI64 *)simrt_gc_alloc(SIMRT_GC_ARRAY_TEXT, (int64_t)total);
    if (dst == NULL) {
        SIMRT_GC_DEFER_LEAVE();
        fprintf(stderr, "sim: out of memory copying text array\n");
        abort();
    }
    dst->ndims = src->ndims;
    memcpy(dst->bounds_and_data, src->bounds_and_data, header - sizeof(int64_t));
    SimrtTextFrame *const *src_data =
        (SimrtTextFrame *const *)(src->bounds_and_data + (size_t)src->ndims * 2);
    SimrtTextFrame **dst_data = simrt_array_text_data(dst);
    for (int64_t i = 0; i < count; i++) {
        dst_data[i] = simrt_text_copy(src_data[i]);
    }
    SIMRT_GC_DEFER_LEAVE();
    return dst;
}

void *simrt_array_load_text(simrt_gc_ptr array_ptr, int64_t ndims, const int64_t *indices) {
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
    SimrtTextFrame *frame =
        ((SimrtTextFrame *const *)(array->bounds_and_data + (size_t)array->ndims * 2))[linear];
    if (frame == NULL) {
        return simrt_text_notext();
    }
    return frame;
}

/* Independent frame descriptor sharing `src`'s object (text `:-` into a new
 * slot). Needed so later `variable :- …` (which mutates the variable's frame
 * in place) cannot rewrite every array element that previously stored the
 * same frame pointer. */
static SimrtTextFrame *simrt_text_clone_ref(SimrtTextFrame *src) {
    SimrtTextFrame *frame = simrt_text_alloc_frame();
    if (src == NULL || simrt_text_frame_is_notext(src)) {
        frame->obj = NULL;
        frame->start = 1;
        frame->length = 0;
        frame->pos = 1;
        return frame;
    }
    frame->obj = src->obj;
    frame->start = src->start;
    frame->length = src->length;
    frame->pos = src->pos;
    return frame;
}

void simrt_array_store_text(
    simrt_gc_ptr array_ptr, int64_t ndims, const int64_t *indices, SimrtTextFrame *frame
) {
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
    simrt_array_text_data(array)[linear] =
        simrt_text_clone_ref(frame);
}

SimrtTextFrame *simrt_text_from_literal(const unsigned char *ptr, size_t len) {
    SimrtTextFrame *frame;
    if (ptr == NULL || len == 0) {
        return simrt_text_notext();
    }
    SIMRT_GC_DEFER_BEGIN();
    frame = simrt_text_alloc_frame();
    frame->obj = simrt_text_alloc_object(len, 1);
    simrt_text_write_object(frame->obj, 1, ptr, len);
    frame->start = 1;
    frame->length = (int64_t)len;
    frame->pos = 1;
    SIMRT_GC_DEFER_LEAVE();
    return frame;
}

SimrtTextFrame *simrt_datetime(void) {
    time_t now = time(NULL);
    struct tm local;
    struct timespec ts;
    int millis = 0;
    char buf[32];
    size_t len;
#if defined(_WIN32)
    localtime_s(&local, &now);
#else
    localtime_r(&now, &local);
#endif
#if defined(CLOCK_REALTIME) && !defined(_WIN32)
    if (clock_gettime(CLOCK_REALTIME, &ts) == 0) {
        millis = (int)(ts.tv_nsec / 1000000L);
    }
#else
    (void)ts;
#endif
    len = (size_t)snprintf(
        buf,
        sizeof(buf),
        "%04d-%02d-%02d %02d:%02d:%02d.%03d",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday,
        local.tm_hour,
        local.tm_min,
        local.tm_sec,
        millis
    );
    return simrt_text_from_literal((const unsigned char *)buf, len);
}

SimrtTextFrame *simrt_text_copy(SimrtTextFrame *src) {
    SimrtTextFrame *frame;
    size_t len;
    const unsigned char *content;
    if (simrt_text_frame_is_notext(src)) {
        return simrt_text_notext();
    }
    len = simrt_text_content_length(src);
    content = simrt_text_content_ptr(src);
    SIMRT_GC_DEFER_BEGIN();
    frame = simrt_text_alloc_frame();
    frame->obj = simrt_text_alloc_object(len, 0);
    simrt_text_write_object(frame->obj, 1, content, len);
    frame->start = 1;
    frame->length = (int64_t)len;
    frame->pos = 1;
    SIMRT_GC_DEFER_LEAVE();
    return frame;
}

SimrtTextFrame *simrt_text_blanks(int64_t n) {
    SimrtTextFrame *frame;
    size_t len;
    if (n < 0) {
        fprintf(stderr, "sim: parameter to blanks < 0\n");
        abort();
    }
    if (n == 0) {
        return simrt_text_notext();
    }
    len = simrt_text_checked_size(n);
    SIMRT_GC_DEFER_BEGIN();
    frame = simrt_text_alloc_frame();
    frame->obj = simrt_text_alloc_object(len, 0);
    memset(frame->obj->main, ' ', len);
    frame->start = 1;
    frame->length = n;
    frame->pos = 1;
    SIMRT_GC_DEFER_LEAVE();
    return frame;
}

void simrt_text_assign_ref(SimrtTextFrame *dest, SimrtTextFrame *src) {
    if (dest == NULL) {
        return;
    }
    /* Reference assignment copies START/LENGTH/POS from the source (Standard §8.3). */
    if (simrt_text_frame_is_notext(src)) {
        dest->obj = NULL;
        dest->start = 1;
        dest->length = 0;
        dest->pos = 1;
        return;
    }
    dest->obj = src->obj;
    dest->start = src->start;
    dest->length = src->length;
    dest->pos = src->pos;
}

int simrt_text_content_eq(SimrtTextFrame *a, SimrtTextFrame *b) {
    size_t a_len = simrt_text_content_length(a);
    size_t b_len = simrt_text_content_length(b);
    if (a_len != b_len) {
        return 0;
    }
    if (a_len == 0) {
        return 1;
    }
    return memcmp(simrt_text_content_ptr(a), simrt_text_content_ptr(b), a_len) == 0;
}

/* Lexicographic content ordering for text ranking (`<` / `<=` / `>` / `>=`). */
int64_t simrt_text_content_cmp(SimrtTextFrame *a, SimrtTextFrame *b) {
    size_t a_len = simrt_text_content_length(a);
    size_t b_len = simrt_text_content_length(b);
    size_t n = a_len < b_len ? a_len : b_len;
    if (n > 0) {
        int c = memcmp(simrt_text_content_ptr(a), simrt_text_content_ptr(b), n);
        if (c < 0) {
            return -1;
        }
        if (c > 0) {
            return 1;
        }
    }
    if (a_len < b_len) {
        return -1;
    }
    if (a_len > b_len) {
        return 1;
    }
    return 0;
}

/* Simula text reference equality: same object + start + length (both notext match). */
int simrt_text_ref_eq(SimrtTextFrame *a, SimrtTextFrame *b) {
    int a_notext = simrt_text_frame_is_notext(a);
    int b_notext = simrt_text_frame_is_notext(b);
    if (a_notext && b_notext) {
        return 1;
    }
    if (a_notext || b_notext) {
        return 0;
    }
    return a->obj == b->obj && a->start == b->start && a->length == b->length;
}

SimrtTextFrame *simrt_text_concat(SimrtTextFrame *left, SimrtTextFrame *right) {
    SimrtTextFrame *frame;
    size_t left_len = simrt_text_content_length(left);
    size_t right_len = simrt_text_content_length(right);
    size_t total;
    if (left_len == 0 && right_len == 0) {
        return simrt_text_notext();
    }
    if (!simrt_host_size_add(left_len, right_len, &total)) {
        simrt_text_oom();
    }
    SIMRT_GC_DEFER_BEGIN();
    frame = simrt_text_alloc_frame();
    frame->obj = simrt_text_alloc_object(total, 0);
    if (left_len > 0) {
        simrt_text_write_object(frame->obj, 1, simrt_text_content_ptr(left), left_len);
    }
    if (right_len > 0) {
        simrt_text_write_object(frame->obj, (int64_t)(1 + left_len), simrt_text_content_ptr(right), right_len);
    }
    frame->start = 1;
    frame->length = (int64_t)total;
    frame->pos = 1;
    SIMRT_GC_DEFER_LEAVE();
    return frame;
}

void simrt_text_assign_value(SimrtTextFrame *dest, SimrtTextFrame *src) {
    if (dest == NULL) {
        return;
    }
    if (simrt_text_frame_is_notext(dest) && simrt_text_frame_is_notext(src)) {
        dest->pos = 1;
        return;
    }
    if (simrt_text_frame_is_notext(dest)) {
        /* Match interpreter `TextFrame` clone into a notext destination:
         * share the source object (preserving `constant`) rather than
         * duplicating characters into a fresh mutable buffer. */
        if (simrt_text_frame_is_notext(src)) {
            dest->obj = NULL;
            dest->start = 1;
            dest->length = 0;
            dest->pos = 1;
        } else {
            dest->obj = src->obj;
            dest->start = src->start;
            dest->length = src->length;
            dest->pos = src->pos;
        }
        return;
    }
    if (simrt_text_frame_is_notext(src)) {
        size_t dest_len = simrt_text_checked_size(dest->length);
        unsigned char *spaces = (unsigned char *)simrt_text_host_alloc(dest_len);
        memset(spaces, ' ', dest_len);
        simrt_text_write_object(dest->obj, dest->start, spaces, dest_len);
        free(spaces);
        /* Match interpreter: value assignment does not alter POS. */
        return;
    }
    size_t src_len = simrt_text_content_length(src);
    if ((int64_t)src_len > dest->length) {
        fprintf(stderr, "sim: text assignment exceeds destination length\n");
        abort();
    }
    if (dest->obj != NULL && dest->obj->constant) {
        fprintf(stderr, "sim: assignment to constant text frame\n");
        abort();
    }
    size_t dest_len = simrt_text_checked_size(dest->length);
    unsigned char *buffer = (unsigned char *)simrt_text_host_alloc(dest_len);
    memset(buffer, ' ', dest_len);
    memcpy(buffer, simrt_text_content_ptr(src), src_len);
    simrt_text_write_object(dest->obj, dest->start, buffer, dest_len);
    free(buffer);
    /* Match interpreter / DosTestBatch: value assignment leaves POS unchanged. */
}

void simrt_text_content_ptr_len(SimrtTextFrame *frame, const unsigned char **ptr_out, int64_t *len_out) {
    if (ptr_out != NULL) {
        *ptr_out = simrt_text_content_ptr(frame);
    }
    if (len_out != NULL) {
        *len_out = (int64_t)simrt_text_content_length(frame);
    }
}

#if defined(_WIN32)
#define SIMRT_THREAD_LOCAL __declspec(thread)
#else
#define SIMRT_THREAD_LOCAL __thread
#endif

static SIMRT_THREAD_LOCAL unsigned char *simrt_utf8_scratch;
static SIMRT_THREAD_LOCAL size_t simrt_utf8_scratch_cap;

static void simrt_utf8_scratch_reserve(size_t need) {
    if (need <= simrt_utf8_scratch_cap) {
        return;
    }
    unsigned char *next = (unsigned char *)realloc(simrt_utf8_scratch, need);
    if (next == NULL) {
        simrt_text_oom();
    }
    simrt_utf8_scratch = next;
    simrt_utf8_scratch_cap = need;
}

void simrt_text_utf8_ptr_len(SimrtTextFrame *frame, const unsigned char **ptr_out, int64_t *len_out) {
    const unsigned char *src = simrt_text_content_ptr(frame);
    size_t n = simrt_text_content_length(frame);
    if (n == 0) {
        if (ptr_out != NULL) {
            *ptr_out = (const unsigned char *)"";
        }
        if (len_out != NULL) {
            *len_out = 0;
        }
        return;
    }
    simrt_utf8_scratch_reserve(n * 2);
    size_t out = 0;
    for (size_t i = 0; i < n; i++) {
        unsigned c = src[i];
        if (c < 0x80u) {
            simrt_utf8_scratch[out++] = (unsigned char)c;
        } else {
            simrt_utf8_scratch[out++] = (unsigned char)(0xC0u | (c >> 6));
            simrt_utf8_scratch[out++] = (unsigned char)(0x80u | (c & 0x3Fu));
        }
    }
    if (ptr_out != NULL) {
        *ptr_out = simrt_utf8_scratch;
    }
    if (len_out != NULL) {
        *len_out = (int64_t)out;
    }
}

SimrtTextFrame *simrt_text_from_utf8(const unsigned char *ptr, size_t len) {
    if (ptr == NULL || len == 0) {
        return simrt_text_notext();
    }
    unsigned char *ranks = (unsigned char *)malloc(len);
    if (ranks == NULL) {
        simrt_text_oom();
    }
    size_t nchars = 0;
    size_t i = 0;
    while (i < len) {
        unsigned b = ptr[i];
        if (b < 0x80u) {
            ranks[nchars++] = (unsigned char)b;
            i += 1;
        } else if ((b == 0xC2u || b == 0xC3u) && i + 1 < len) {
            unsigned b2 = ptr[i + 1];
            if ((b2 & 0xC0u) != 0x80u) {
                free(ranks);
                simrt_error("invalid UTF-8 in foreign text");
            }
            ranks[nchars++] = (unsigned char)(((b & 0x1Fu) << 6) | (b2 & 0x3Fu));
            i += 2;
        } else {
            free(ranks);
            simrt_error("invalid UTF-8 in foreign text");
        }
    }
    SimrtTextFrame *frame = simrt_text_from_literal(ranks, nchars);
    free(ranks);
    return frame;
}

int simrt_text_is_notext(SimrtTextFrame *frame) {
    return simrt_text_frame_is_notext(frame);
}

/* Text frame attributes (Standard Chapter 8), matching `src/runtime/text.rs`. */
int64_t simrt_text_length(SimrtTextFrame *frame) {
    if (frame == NULL) {
        return 0;
    }
    return frame->length;
}

int64_t simrt_text_constant(SimrtTextFrame *frame) {
    /* NOTEXT is constant (Standard Chapter 8). */
    if (frame == NULL || simrt_text_frame_is_notext(frame) || frame->obj == NULL) {
        return 1;
    }
    return frame->obj->constant ? 1 : 0;
}

int64_t simrt_text_start(SimrtTextFrame *frame) {
    if (frame == NULL || simrt_text_frame_is_notext(frame)) {
        return 1;
    }
    return frame->start;
}

SimrtTextFrame *simrt_text_main(SimrtTextFrame *src) {
    if (simrt_text_frame_is_notext(src) || src->obj == NULL) {
        return simrt_text_notext();
    }
    SimrtTextFrame *frame = simrt_text_alloc_frame();
    frame->obj = src->obj;
    frame->start = 1;
    frame->length = (int64_t)src->obj->main_len;
    frame->pos = 1;
    return frame;
}

int64_t simrt_text_pos(SimrtTextFrame *frame) {
    if (frame == NULL) {
        return 1;
    }
    return frame->pos;
}

void simrt_text_setpos(SimrtTextFrame *frame, int64_t i) {
    if (frame == NULL) {
        return;
    }
    if (i < 1 || i > frame->length + 1) {
        frame->pos = frame->length + 1;
    } else {
        frame->pos = i;
    }
}

int64_t simrt_text_more(SimrtTextFrame *frame) {
    if (frame == NULL) {
        return 0;
    }
    return frame->pos <= frame->length ? 1 : 0;
}

int64_t simrt_text_getchar(SimrtTextFrame *frame) {
    if (frame == NULL || frame->pos > frame->length) {
        simrt_error("pos out of range");
    }
    if (simrt_text_frame_is_notext(frame)) {
        simrt_error("pos out of range");
    }
    size_t index = (size_t)(frame->start + frame->pos - 2);
    if (index >= frame->obj->main_len) {
        simrt_error("pos out of range");
    }
    int64_t codepoint = (int64_t)frame->obj->main[index];
    frame->pos += 1;
    return codepoint;
}

void simrt_text_putchar(SimrtTextFrame *frame, int64_t ch) {
    if (frame == NULL || simrt_text_frame_is_notext(frame) || frame->obj == NULL
        || frame->obj->constant) {
        simrt_error("putchar on notext or constant text");
    }
    if (frame->pos > frame->length) {
        simrt_error("pos out of range");
    }
    size_t index = (size_t)(frame->start + frame->pos - 2);
    if (index >= frame->obj->main_len) {
        simrt_error("pos out of range");
    }
    frame->obj->main[index] = (unsigned char)(ch & 0xFF);
    frame->pos += 1;
}

SimrtTextFrame *simrt_text_sub(SimrtTextFrame *src, int64_t i, int64_t n) {
    int64_t limit;
    if (src == NULL || i < 0 || n < 0 || src->length < 0 || src->length > INT64_MAX - 1) {
        simrt_error("sub out of frame");
    }
    limit = src->length + 1;
    if (i > limit || n > limit - i) {
        simrt_error("sub out of frame");
    }
    if (n == 0) {
        return simrt_text_notext();
    }
    SimrtTextFrame *frame = simrt_text_alloc_frame();
    frame->obj = src->obj;
    frame->start = src->start + i - 1;
    frame->length = n;
    frame->pos = 1;
    return frame;
}

SimrtTextFrame *simrt_text_strip(SimrtTextFrame *src) {
    if (simrt_text_frame_is_notext(src)) {
        return simrt_text_notext();
    }
    const unsigned char *content = simrt_text_content_ptr(src);
    size_t len = simrt_text_content_length(src);
    while (len > 0 && content[len - 1] == ' ') {
        len--;
    }
    if (len == 0) {
        return simrt_text_notext();
    }
    return simrt_text_sub(src, 1, (int64_t)len);
}

static void simrt_text_abort_item(const char *message) {
    simrt_error(message);
}

size_t simrt_format_real_ex(
    char *buf, size_t cap, double value, int64_t n, int exp_digits
);

/* Parse INTEGER-ITEM: SIGN-PART DIGITS with SIGN-PART = BLANKS [SIGN] BLANKS. */
int simrt_text_parse_integer_item(
    const unsigned char *content,
    size_t len,
    int64_t *value_out,
    size_t *consumed_out
) {
    size_t index = 0;
    int negative = 0;
    size_t number_start;
    char token[64];
    size_t token_len = 0;
    while (index < len && (content[index] == ' ' || content[index] == '\t')) {
        index += 1;
    }
    if (index < len && (content[index] == '+' || content[index] == '-')) {
        negative = content[index] == '-';
        index += 1;
    }
    while (index < len && (content[index] == ' ' || content[index] == '\t')) {
        index += 1;
    }
    number_start = index;
    while (index < len && isdigit((unsigned char)content[index])) {
        index += 1;
    }
    if (index == number_start) {
        return 0;
    }
    if (negative) {
        token[token_len++] = '-';
    }
    if (token_len + (index - number_start) >= sizeof(token)) {
        return 0;
    }
    memcpy(token + token_len, content + number_start, index - number_start);
    token_len += index - number_start;
    token[token_len] = '\0';
    errno = 0;
    {
        char *end = NULL;
        long long parsed = strtoll(token, &end, 10);
        if (errno != 0 || end == token || *end != '\0') {
            simrt_text_abort_item("integer out of range");
        }
        *value_out = (int64_t)parsed;
    }
    *consumed_out = index;
    return 1;
}

static void simrt_text_edit_numeric(SimrtTextFrame *frame, const char *item) {
    if (frame == NULL || simrt_text_frame_is_notext(frame)) {
        simrt_text_abort_item("edit on notext or constant text");
    }
    if (frame->obj != NULL && frame->obj->constant) {
        simrt_text_abort_item("edit on notext or constant text");
    }
    size_t item_len = strlen(item);
    size_t width = simrt_text_checked_size(frame->length);
    if (item_len > width) {
        unsigned char *stars = (unsigned char *)simrt_text_host_alloc(width);
        memset(stars, '*', width);
        simrt_text_write_object(frame->obj, frame->start, stars, width);
        free(stars);
    } else {
        unsigned char *padded = (unsigned char *)simrt_text_host_alloc(width);
        size_t pad = width - item_len;
        memset(padded, ' ', pad);
        memcpy(padded + pad, item, item_len);
        simrt_text_write_object(frame->obj, frame->start, padded, width);
        free(padded);
    }
    frame->pos = frame->length + 1;
}

int64_t simrt_text_getint(SimrtTextFrame *frame) {
    if (frame == NULL || simrt_text_frame_is_notext(frame)) {
        simrt_text_abort_item("no numeric item");
    }
    const unsigned char *content = simrt_text_content_ptr(frame);
    size_t len = simrt_text_content_length(frame);
    int64_t value = 0;
    size_t consumed = 0;
    if (!simrt_text_parse_integer_item(content, len, &value, &consumed)) {
        simrt_text_abort_item("no numeric item");
    }
    frame->pos = (int64_t)consumed + 1;
    return value;
}

void simrt_text_putint(SimrtTextFrame *frame, int64_t value) {
    char buffer[32];
    snprintf(buffer, sizeof(buffer), "%lld", (long long)value);
    simrt_text_edit_numeric(frame, buffer);
}

/* GROUPED-ITEM (§8.5). Sign is applied; trailing blanks before a separated
 * decimal mark are not consumed. */
static int simrt_text_parse_grouped_item(
    const unsigned char *content,
    size_t len,
    int64_t *value_out,
    size_t *consumed_out
) {
    size_t index = 0;
    int negative = 0;
    size_t start;
    int saw_digit = 0;
    char digits[64];
    size_t digits_len = 0;

    while (index < len && (content[index] == ' ' || content[index] == '\t')) {
        index += 1;
    }
    if (index < len && (content[index] == '+' || content[index] == '-')) {
        negative = content[index] == '-';
        index += 1;
    }
    while (index < len && (content[index] == ' ' || content[index] == '\t')) {
        index += 1;
    }

    start = index;
    if (index < len
        && (content[index] == (unsigned char)g_decimal_mark || content[index] == '.'
            || content[index] == ',')) {
        index += 1;
        if (index >= len || !isdigit(content[index])) {
            return 0;
        }
    } else if (index >= len || !isdigit(content[index])) {
        return 0;
    }

    while (index < len && isdigit(content[index])) {
        saw_digit = 1;
        index += 1;
    }
    for (;;) {
        size_t look = index;
        while (look < len && (content[look] == ' ' || content[look] == '\t')) {
            look += 1;
        }
        if (look > index && look < len && isdigit(content[look])) {
            index = look;
            while (index < len && isdigit(content[index])) {
                saw_digit = 1;
                index += 1;
            }
        } else {
            break;
        }
    }
    if (index < len
        && (content[index] == (unsigned char)g_decimal_mark || content[index] == '.'
            || content[index] == ',')) {
        size_t before_mark = index;
        size_t after_mark = index + 1;
        size_t look = after_mark;
        /* Optional `[ DECIMAL-MARK GROUPS ]` — require digits immediately. */
        if (look < len && isdigit(content[look])) {
            index = after_mark;
            while (index < len && isdigit(content[index])) {
                saw_digit = 1;
                index += 1;
            }
            for (;;) {
                look = index;
                while (look < len && (content[look] == ' ' || content[look] == '\t')) {
                    look += 1;
                }
                if (look > index && look < len && isdigit(content[look])) {
                    index = look;
                    while (index < len && isdigit(content[index])) {
                        saw_digit = 1;
                        index += 1;
                    }
                } else {
                    break;
                }
            }
        } else {
            index = before_mark;
        }
    }
    if (!saw_digit) {
        return 0;
    }
    for (size_t i = start; i < index; i++) {
        if (isdigit(content[i])) {
            if (digits_len + 1 >= sizeof(digits)) {
                return 0;
            }
            digits[digits_len++] = (char)content[i];
        }
    }
    digits[digits_len] = '\0';
    errno = 0;
    {
        char *end = NULL;
        long long parsed = strtoll(digits, &end, 10);
        if (errno != 0 || end == digits || *end != '\0') {
            simrt_text_abort_item("grouped item out of range");
        }
        *value_out = negative ? -(int64_t)parsed : (int64_t)parsed;
    }
    *consumed_out = index;
    return 1;
}

int64_t simrt_text_getfrac(SimrtTextFrame *frame) {
    if (frame == NULL || simrt_text_frame_is_notext(frame)) {
        simrt_text_abort_item("no numeric item");
    }
    const unsigned char *content = simrt_text_content_ptr(frame);
    size_t len = simrt_text_content_length(frame);
    int64_t value = 0;
    size_t consumed = 0;
    if (!simrt_text_parse_grouped_item(content, len, &value, &consumed)) {
        simrt_text_abort_item("no numeric item");
    }
    frame->pos = (int64_t)consumed + 1;
    return value;
}

/* Match `runtime/text.rs::group_integer_part` + `group_fractional_part`. */
void simrt_text_group_integer_part(const char *integer, char *out, size_t out_sz) {
    size_t len = strlen(integer);
    if (len <= 3 || out_sz == 0) {
        snprintf(out, out_sz, "%s", integer);
        return;
    }
    size_t first = len % 3;
    if (first == 0) {
        first = 3;
    }
    size_t pos = 0;
    for (size_t i = 0; i < first && pos + 1 < out_sz; i++) {
        out[pos++] = integer[i];
    }
    size_t index = first;
    while (index < len && pos + 4 < out_sz) {
        out[pos++] = ' ';
        out[pos++] = integer[index];
        out[pos++] = integer[index + 1];
        out[pos++] = integer[index + 2];
        index += 3;
    }
    out[pos] = '\0';
}

void simrt_text_group_fractional_part(const char *fraction, char *out, size_t out_sz) {
    size_t len = strlen(fraction);
    size_t pos = 0;
    size_t index = 0;
    if (out_sz == 0) {
        return;
    }
    if (len <= 3) {
        snprintf(out, out_sz, "%s", fraction);
        return;
    }
    while (index < len && pos + 1 < out_sz) {
        if (index > 0 && index % 3 == 0) {
            if (pos + 1 >= out_sz) {
                break;
            }
            out[pos++] = ' ';
        }
        out[pos++] = fraction[index++];
    }
    out[pos] = '\0';
}

void simrt_text_putfrac(SimrtTextFrame *frame, int64_t value, int64_t places) {
    /* Standard §8.7: n<=0 → GROUPED ITEM with no decimal mark. Extreme
     * negative n (simtst18 uses -800) still edits the integer digits of i
     * when i*10**(-n) cannot fit a practical buffer. */
    if (places < 0) {
        places = 0;
    }
    int negative = value < 0;
    uint64_t abs_value = negative ? (uint64_t)(-(value + 1)) + 1 : (uint64_t)value;
    uint64_t scale = 1;
    for (int64_t i = 0; i < places; i++) {
        scale *= 10;
    }
    uint64_t whole = places == 0 ? abs_value : abs_value / scale;
    uint64_t frac = places == 0 ? 0 : abs_value % scale;

    char whole_digits[32];
    char frac_digits[32];
    char grouped_whole[64];
    char grouped_frac[64];
    char formatted[96];

    snprintf(whole_digits, sizeof(whole_digits), "%llu", (unsigned long long)whole);
    simrt_text_group_integer_part(whole_digits, grouped_whole, sizeof(grouped_whole));

    if (places > 0) {
        snprintf(frac_digits, sizeof(frac_digits), "%0*llu", (int)places, (unsigned long long)frac);
        simrt_text_group_fractional_part(frac_digits, grouped_frac, sizeof(grouped_frac));
        if (whole == 0) {
            if (negative) {
                snprintf(formatted, sizeof(formatted), "-%c%s", g_decimal_mark, grouped_frac);
            } else {
                snprintf(formatted, sizeof(formatted), "%c%s", g_decimal_mark, grouped_frac);
            }
        } else if (negative) {
            snprintf(
                formatted, sizeof(formatted), "-%s%c%s", grouped_whole, g_decimal_mark, grouped_frac
            );
        } else {
            snprintf(
                formatted, sizeof(formatted), "%s%c%s", grouped_whole, g_decimal_mark, grouped_frac
            );
        }
    } else if (negative) {
        snprintf(formatted, sizeof(formatted), "-%s", grouped_whole);
    } else {
        snprintf(formatted, sizeof(formatted), "%s", grouped_whole);
    }
    simrt_text_edit_numeric(frame, formatted);
}

/* Match `runtime/text.rs::parse_real_item_with`: SIGN-PART allows blanks
 * around an optional sign (simtst18: `"   -  12.34&-10"`). */
int simrt_text_parse_real_item(
    const unsigned char *content,
    size_t len,
    double *value_out,
    size_t *consumed_out
) {
    size_t index = 0;
    size_t token_len = 0;
    char token[160];
    int saw_digit = 0;
    while (index < len && (content[index] == ' ' || content[index] == '\t')) {
        index += 1;
    }
    if (index < len && (content[index] == '+' || content[index] == '-')) {
        if (token_len + 1 >= sizeof(token)) {
            return 0;
        }
        token[token_len++] = (char)content[index];
        index += 1;
    }
    while (index < len && (content[index] == ' ' || content[index] == '\t')) {
        index += 1;
    }
    size_t body_start = index;
    while (index < len && isdigit(content[index])) {
        saw_digit = 1;
        index += 1;
    }
    if (index < len
        && (content[index] == '.' || content[index] == ','
            || content[index] == (unsigned char)g_decimal_mark)) {
        index += 1;
        while (index < len && isdigit(content[index])) {
            saw_digit = 1;
            index += 1;
        }
    }
    if (index < len
        && (content[index] == (unsigned char)g_lowten || content[index] == '&'
            || content[index] == 'e' || content[index] == 'E')) {
        index += 1;
        while (index < len && (content[index] == ' ' || content[index] == '\t')) {
            index += 1;
        }
        if (index < len && (content[index] == '+' || content[index] == '-')) {
            index += 1;
        }
        while (index < len && (content[index] == ' ' || content[index] == '\t')) {
            index += 1;
        }
        size_t exp_start = index;
        while (index < len && isdigit(content[index])) {
            saw_digit = 1;
            index += 1;
        }
        if (index == exp_start) {
            return 0;
        }
    }
    if (!saw_digit) {
        return 0;
    }
    /* Build a strtod token, dropping blanks inserted by SIGN-PART. */
    for (size_t j = body_start; j < index; j++) {
        char ch = (char)content[j];
        if (ch == ' ' || ch == '\t') {
            continue;
        }
        if (token_len + 1 >= sizeof(token)) {
            return 0;
        }
        if (ch == g_decimal_mark || ch == ',') {
            token[token_len++] = '.';
        } else if (ch == g_lowten || ch == '&' || ch == 'e' || ch == 'E') {
            token[token_len++] = 'e';
        } else {
            token[token_len++] = ch;
        }
    }
    token[token_len] = '\0';
    errno = 0;
    {
        char *end = NULL;
        double parsed = strtod(token, &end);
        /* Underflow (denormals / zero) is accepted; only true overflow / junk abort. */
        if (end == token || *end != '\0') {
            simrt_text_abort_item("real out of range");
        }
        if (errno == ERANGE && (parsed == HUGE_VAL || parsed == -HUGE_VAL)) {
            simrt_text_abort_item("real out of range");
        }
        *value_out = parsed;
    }
    *consumed_out = index;
    return 1;
}

double simrt_text_getreal(SimrtTextFrame *frame) {
    if (frame == NULL || simrt_text_frame_is_notext(frame)) {
        simrt_text_abort_item("no numeric item");
    }
    const unsigned char *content = simrt_text_content_ptr(frame);
    size_t len = simrt_text_content_length(frame);
    double value = 0.0;
    size_t consumed = 0;
    if (!simrt_text_parse_real_item(content, len, &value, &consumed)) {
        simrt_text_abort_item("no numeric item");
    }
    frame->pos = (int64_t)consumed + 1;
    return value;
}

void simrt_text_putfix(SimrtTextFrame *frame, double value, int64_t places) {
    if (places < 0) {
        simrt_text_abort_item("putfix: n < 0");
    }
    /* Normalize -0 so putfix/putreal match DosTestBatch expectations.
     * Use copysign: `value = 0.0` is a no-op under IEEE equality and gets
     * optimized away by clang/gcc, leaving snprintf to emit "-0...". */
    if (value == 0.0) {
        value = copysign(0.0, 1.0);
    }
    char buffer[128];
    if (places == 0) {
        snprintf(buffer, sizeof(buffer), "%.0f", round(value));
    } else {
        snprintf(buffer, sizeof(buffer), "%.*f", (int)places, value);
    }
    if (g_decimal_mark != '.') {
        for (char *p = buffer; *p; p++) {
            if (*p == '.') {
                *p = g_decimal_mark;
            }
        }
    }
    simrt_text_edit_numeric(frame, buffer);
}

void simrt_text_putreal_ex(
    SimrtTextFrame *frame, double value, int64_t n, int64_t exp_digits
) {
    if (n < 0) {
        simrt_text_abort_item("putreal: n < 0");
    }
    if (value == 0.0) {
        value = copysign(0.0, 1.0);
    }
    char buffer[128];
    int digits = exp_digits < 2 ? 2 : (exp_digits > 8 ? 8 : (int)exp_digits);
    buffer[0] = '\0';
    /* Match Rust `format_scientific_item` / outreal: signed, zero-padded exponent. */
    simrt_format_real_ex(buffer, sizeof(buffer), value, n, digits);
    for (char *p = buffer; *p; p++) {
        if (*p == '.') {
            *p = g_decimal_mark;
        } else if (*p == 'e' || *p == 'E') {
            *p = g_lowten;
        }
    }
    if (g_lowten != '&') {
        for (char *p = buffer; *p; p++) {
            if (*p == '&') {
                *p = g_lowten;
            }
        }
    }
    simrt_text_edit_numeric(frame, buffer);
}

void simrt_text_putreal(SimrtTextFrame *frame, double value, int64_t n) {
    simrt_text_putreal_ex(frame, value, n, 2);
}

static void simrt_text_abort_case_fold(const char *what) {
    fprintf(stderr, "sim: %s on notext or constant text\n", what);
    abort();
}

void simrt_text_upcase(SimrtTextFrame *frame) {
    if (frame == NULL || simrt_text_frame_is_notext(frame)) {
        simrt_text_abort_case_fold("upcase");
    }
    if (frame->obj->constant) {
        simrt_text_abort_case_fold("upcase");
    }
    frame->pos = 1;
    unsigned char *content = frame->obj->main + (size_t)(frame->start - 1);
    for (int64_t i = 0; i < frame->length; i++) {
        content[i] = (unsigned char)toupper(content[i]);
    }
}

void simrt_text_lowcase(SimrtTextFrame *frame) {
    if (frame == NULL || simrt_text_frame_is_notext(frame)) {
        simrt_text_abort_case_fold("lowcase");
    }
    if (frame->obj->constant) {
        simrt_text_abort_case_fold("lowcase");
    }
    frame->pos = 1;
    unsigned char *content = frame->obj->main + (size_t)(frame->start - 1);
    for (int64_t i = 0; i < frame->length; i++) {
        content[i] = (unsigned char)tolower(content[i]);
    }
}

int64_t simrt_error_text(SimrtTextFrame *frame) {
    size_t len = simrt_text_content_length(frame);
    const unsigned char *ptr = simrt_text_content_ptr(frame);
    fprintf(stderr, "sim: ");
    if (ptr != NULL && len > 0) {
        fwrite(ptr, 1, len, stderr);
    }
    fputc('\n', stderr);
    exit(1);
    return 0;
}


/* `outreal`/`outfix`/`outfrac` (§10.5.8) — thin MVP: approximates
 * `TextFrame::edit_put{real,fix,frac}` (src/runtime/text.rs) without full
 * asterisk-fill-on-overflow or split-field negative-width semantics. Always
 * uses the ENVIRONMENT defaults (decimalmark '.', lowten '&'). */
size_t simrt_clamp_len(int len, size_t cap) {
    if (len <= 0) {
        return 0;
    }
    return (size_t)len < cap ? (size_t)len : cap - 1;
}

size_t simrt_format_fix(char *buf, size_t cap, double value, int64_t n) {
    int len;
    /* Clamp decimal-place count: absurd `n` would overflow `buf` via `%.*f`. */
    int places = n > 0 ? (int)(n > 60 ? 60 : n) : 0;
    if (value == 0.0) {
        value = copysign(0.0, 1.0);
    }
    if (n <= 0) {
        len = snprintf(buf, cap, "%lld", (long long)llround(value));
    } else {
        len = snprintf(buf, cap, "%.*f", places, value);
    }
    return simrt_clamp_len(len, cap);
}

/* Scientific `putreal`/`outreal` item. `exp_digits` is 2 for REAL and 3 for
 * LONG REAL (DosTestBatch simtst28). Falls back to snprintf `%e` when the
 * platform already emits a signed exponent of that width. */
size_t simrt_format_real_ex(
    char *buf, size_t cap, double value, int64_t n, int exp_digits
) {
    int digits = n > 1 ? (int)(n - 1 > 60 ? 60 : n - 1) : 0;
    int len;
    char tmp[128];
    char *amp;
    long exp_val;
    int mant_len;
    if (value == 0.0) {
        value = copysign(0.0, 1.0);
    }
    if (exp_digits < 2) {
        exp_digits = 2;
    }
    if (exp_digits > 8) {
        exp_digits = 8;
    }
    len = snprintf(tmp, sizeof(tmp), "%.*e", digits, value);
    if (len < 0) {
        if (cap > 0) {
            buf[0] = '\0';
        }
        return 0;
    }
    if ((size_t)len >= sizeof(tmp)) {
        len = (int)sizeof(tmp) - 1;
    }
    amp = strchr(tmp, 'e');
    if (amp == NULL) {
        amp = strchr(tmp, 'E');
    }
    if (amp == NULL) {
        return simrt_clamp_len(
            snprintf(buf, cap, "%s", tmp), cap
        );
    }
    *amp = '\0';
    exp_val = strtol(amp + 1, NULL, 10);
    mant_len = (int)strlen(tmp);
    if (exp_val >= 0) {
        len = snprintf(
            buf, cap, "%s&+%0*ld", tmp, exp_digits, exp_val
        );
    } else {
        len = snprintf(
            buf, cap, "%s&-%0*ld", tmp, exp_digits, -exp_val
        );
    }
    (void)mant_len;
    return simrt_clamp_len(len, cap);
}

size_t simrt_format_frac(char *buf, size_t cap, int64_t value, int64_t n) {
    /* Match `simrt_text_putfrac` / Standard §8.7 GROUPED-ITEM editing so
     * OutFrac and text.putfrac agree (simtst85 expects `.000 1`, not `0.0001`). */
    int negative;
    uint64_t abs_value;
    uint64_t scale = 1;
    uint64_t whole;
    uint64_t frac;
    int64_t places;
    int64_t i;
    char whole_digits[32];
    char frac_digits[32];
    char grouped_whole[64];
    char grouped_frac[64];
    int len;

    if (n <= 0) {
        len = snprintf(buf, cap, "%lld", (long long)value);
        return simrt_clamp_len(len, cap);
    }
    negative = value < 0;
    abs_value = negative ? (uint64_t)(-(value + 1)) + 1 : (uint64_t)value;
    places = n > 18 ? 18 : n;
    for (i = 0; i < places; i++) {
        scale *= 10;
    }
    whole = abs_value / scale;
    frac = abs_value % scale;
    snprintf(whole_digits, sizeof(whole_digits), "%llu", (unsigned long long)whole);
    simrt_text_group_integer_part(whole_digits, grouped_whole, sizeof(grouped_whole));
    snprintf(frac_digits, sizeof(frac_digits), "%0*llu", (int)places, (unsigned long long)frac);
    simrt_text_group_fractional_part(frac_digits, grouped_frac, sizeof(grouped_frac));
    if (whole == 0) {
        if (negative) {
            len = snprintf(buf, cap, "-%c%s", g_decimal_mark, grouped_frac);
        } else {
            len = snprintf(buf, cap, "%c%s", g_decimal_mark, grouped_frac);
        }
    } else if (negative) {
        len = snprintf(
            buf, cap, "-%s%c%s", grouped_whole, g_decimal_mark, grouped_frac
        );
    } else {
        len = snprintf(
            buf, cap, "%s%c%s", grouped_whole, g_decimal_mark, grouped_frac
        );
    }
    return simrt_clamp_len(len, cap);
}


/* Right/left-justifies `item` in a field of width `|w|` (spaces, clamped to
 * `SIMRT_NUMERIC_FIELD_MAX`); `w == 0` emits `item` unpadded. Overflowing
 * items are asterisk-filled, matching `TextFrame::edit_numeric`'s "item too
 * long" handling. */
void simrt_pad_numeric_field(
    unsigned char *out, size_t *out_len, const char *item, size_t item_len, int64_t w
) {
    size_t width;
    size_t pad;
    if (item_len > SIMRT_NUMERIC_FIELD_MAX) {
        item_len = SIMRT_NUMERIC_FIELD_MAX;
    }
    if (w == 0) {
        memcpy(out, item, item_len);
        *out_len = item_len;
        return;
    }
    width = (size_t)(w < 0 ? -w : w);
    if (width > SIMRT_NUMERIC_FIELD_MAX) {
        width = SIMRT_NUMERIC_FIELD_MAX;
    }
    if (item_len > width) {
        memset(out, '*', width);
        *out_len = width;
        return;
    }
    pad = width - item_len;
    if (w > 0) {
        memset(out, ' ', pad);
        memcpy(out + pad, item, item_len);
    } else {
        memcpy(out, item, item_len);
        memset(out + item_len, ' ', pad);
    }
    *out_len = width;
}

char *simrt_text_to_cstr(const SimrtTextFrame *frame) {
    size_t len = simrt_text_content_length(frame);
    const unsigned char *ptr = simrt_text_content_ptr(frame);
    char *buf = (char *)simrt_host_malloc_sum(len, 1);
    if (buf == NULL) {
        simrt_text_oom();
    }
    if (len > 0 && ptr != NULL) {
        memcpy(buf, ptr, len);
    }
    buf[len] = '\0';
    return buf;
}
