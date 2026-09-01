/* Native Simula runtime ABI.
 *
 * Generated code imports these symbols by name (Cranelift `Linkage::Import`).
 * This header is the review contract so C fixtures and the implementation
 * cannot drift.
 *
 * Pointer kinds: `simrt_gc_ptr` is a managed-heap payload (never `free`).
 * `gc-roots:` on an allocator says how a result stays alive until the caller
 * stores it in a precise root frame.
 */

#ifndef SIMRT_RUNTIME_H
#define SIMRT_RUNTIME_H

#include <stddef.h>
#include <stdint.h>

#include "annot.h"
#include "gc.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct SimrtTextFrame SimrtTextFrame;

void simrt_error(const char *message);

/* --- ENVIRONMENT ------------------------------------------------------- */

int64_t simrt_decimalmark(int64_t c);
int64_t simrt_lowten(int64_t c);
int64_t simrt_current_decimalmark(void);
int64_t simrt_current_lowten(void);
double simrt_sqrt(double x);
double simrt_sin(double x);
double simrt_cos(double x);
double simrt_tan(double x);
double simrt_ln(double x);
double simrt_exp(double x);
double simrt_arctan(double x);
double simrt_sinh(double x);
double simrt_cosh(double x);
double simrt_tanh(double x);
double simrt_log10(double x);
int64_t simrt_digit(int64_t c);
int64_t simrt_letter(int64_t c);
int64_t simrt_char(int64_t i);
int64_t simrt_isochar(int64_t i);
int64_t simrt_rank(int64_t c);
int64_t simrt_isorank(int64_t c);
int64_t simrt_max_int(int64_t a, int64_t b);
int64_t simrt_min_int(int64_t a, int64_t b);
double simrt_max_real(double a, double b);
double simrt_min_real(double a, double b);
double simrt_cotan(double x);
double simrt_arcsin(double x);
double simrt_arccos(double x);
double simrt_arctan2(double y, double x);
double simrt_addepsilon(double x);
double simrt_subepsilon(double x);
int64_t simrt_draw(double a, int64_t *stream);
int64_t simrt_randint(int64_t a, int64_t b, int64_t *stream);
double simrt_uniform(double a, double b, int64_t *stream);
double simrt_normal(double a, double b, int64_t *stream);
double simrt_negexp(double a, int64_t *stream);
int64_t simrt_poisson(double a, int64_t *stream);
double simrt_erlang(double a, double b, int64_t *stream);
double simrt_cputime(void);
double simrt_clocktime(void);
int64_t simrt_rem(int64_t i, int64_t j);
int64_t simrt_mod(int64_t i, int64_t j);
int64_t simrt_sign(double r);
double simrt_abs_real(double r);
int64_t simrt_abs_int(int64_t i);
double simrt_f64_pow(double base, double exponent);

/* --- Terminal images --------------------------------------------------- */

void simrt_out_text(const unsigned char *text, size_t length);
void simrt_out_char(int64_t ch);
void simrt_out_image(void);
void simrt_break_out_image(void);
void simrt_in_image(void);
int32_t simrt_in_char(void);
int32_t simrt_endfile(void);
void simrt_out_int(int64_t value, int64_t w);
void simrt_out_real_ex(double value, int64_t n, int64_t w, int64_t exp_digits);
void simrt_out_real(double value, int64_t n, int64_t w);
void simrt_out_fix(double value, int64_t n, int64_t w);
void simrt_out_frac(int64_t value, int64_t n, int64_t w);
SimrtTextFrame *simrt_in_line(void);

/* --- Arrays: gc-roots: none (single alloc) except text flavours (defer) */

SIMRT_MUST_USE simrt_gc_ptr simrt_array_alloc_i64(int64_t ndims, const int64_t *bounds);
SIMRT_MUST_USE simrt_gc_ptr simrt_array_copy_i64(simrt_gc_ptr array_ptr);
int64_t simrt_array_load_i64(simrt_gc_ptr array_ptr, int64_t ndims, const int64_t *indices);
void simrt_array_store_i64(simrt_gc_ptr array_ptr, int64_t ndims, const int64_t *indices, int64_t value);
SIMRT_MUST_USE simrt_gc_ptr simrt_array_alloc_f64(int64_t ndims, const int64_t *bounds);
SIMRT_MUST_USE simrt_gc_ptr simrt_array_copy_f64(simrt_gc_ptr array_ptr);
double simrt_array_load_f64(simrt_gc_ptr array_ptr, int64_t ndims, const int64_t *indices);
void simrt_array_store_f64(simrt_gc_ptr array_ptr, int64_t ndims, const int64_t *indices, double value);
int64_t simrt_array_lowerbound(simrt_gc_ptr array_ptr, int64_t dim_1based);
int64_t simrt_array_upperbound(simrt_gc_ptr array_ptr, int64_t dim_1based);
int64_t simrt_discrete(simrt_gc_ptr array_ptr, int64_t *stream);
int64_t simrt_histd(simrt_gc_ptr array_ptr, int64_t *stream);
double simrt_linear(simrt_gc_ptr a_ptr, simrt_gc_ptr b_ptr, int64_t *stream);
int64_t simrt_histo(simrt_gc_ptr a_ptr, simrt_gc_ptr b_ptr, double c, double d);
/* gc-roots: defer (descriptor plus per-slot notext frames) */
SIMRT_MUST_USE simrt_gc_ptr simrt_array_alloc_text(int64_t ndims, const int64_t *bounds);
SIMRT_MUST_USE simrt_gc_ptr simrt_array_copy_text(simrt_gc_ptr array_ptr);
SIMRT_MUST_USE simrt_gc_ptr simrt_array_load_text(
    simrt_gc_ptr array_ptr, int64_t ndims, const int64_t *indices
);
void simrt_array_store_text(
    simrt_gc_ptr array_ptr, int64_t ndims, const int64_t *indices, SimrtTextFrame *frame
);

/* --- Texts: gc-roots: defer on multi-alloc (literal/copy/blanks/concat);
 *            none on single-frame (notext/sub/strip/main) --------------- */

SIMRT_MUST_USE SimrtTextFrame *simrt_text_notext(void);
SIMRT_MUST_USE SimrtTextFrame *simrt_text_from_literal(const unsigned char *ptr, size_t len);
SIMRT_MUST_USE SimrtTextFrame *simrt_datetime(void);
SIMRT_MUST_USE SimrtTextFrame *simrt_text_copy(SimrtTextFrame *src);
SIMRT_MUST_USE SimrtTextFrame *simrt_text_blanks(int64_t n);
void simrt_text_assign_ref(SimrtTextFrame *dest, SimrtTextFrame *src);
int simrt_text_content_eq(SimrtTextFrame *a, SimrtTextFrame *b);
int64_t simrt_text_content_cmp(SimrtTextFrame *a, SimrtTextFrame *b);
int simrt_text_ref_eq(SimrtTextFrame *a, SimrtTextFrame *b);
SIMRT_MUST_USE SimrtTextFrame *simrt_text_concat(SimrtTextFrame *left, SimrtTextFrame *right);
void simrt_text_assign_value(SimrtTextFrame *dest, SimrtTextFrame *src);
void simrt_text_content_ptr_len(
    SimrtTextFrame *frame, const unsigned char **ptr_out, int64_t *len_out
);
/* FFI-only: encode ranks 0–255 as UTF-8. Pointer is valid until the next
 * `simrt_text_utf8_ptr_len` call on this thread. */
void simrt_text_utf8_ptr_len(
    SimrtTextFrame *frame, const unsigned char **ptr_out, int64_t *len_out
);
/* FFI-only: decode UTF-8 of ranks 0–255. Invalid UTF-8 or a codepoint above
 * 255 is a runtime error. */
SIMRT_MUST_USE SimrtTextFrame *simrt_text_from_utf8(const unsigned char *ptr, size_t len);
int simrt_text_is_notext(SimrtTextFrame *frame);
int64_t simrt_text_length(SimrtTextFrame *frame);
int64_t simrt_text_constant(SimrtTextFrame *frame);
int64_t simrt_text_start(SimrtTextFrame *frame);
SIMRT_MUST_USE SimrtTextFrame *simrt_text_main(SimrtTextFrame *src);
int64_t simrt_text_pos(SimrtTextFrame *frame);
void simrt_text_setpos(SimrtTextFrame *frame, int64_t i);
int64_t simrt_text_more(SimrtTextFrame *frame);
int64_t simrt_text_getchar(SimrtTextFrame *frame);
void simrt_text_putchar(SimrtTextFrame *frame, int64_t ch);
SIMRT_MUST_USE SimrtTextFrame *simrt_text_sub(SimrtTextFrame *src, int64_t i, int64_t n);
SIMRT_MUST_USE SimrtTextFrame *simrt_text_strip(SimrtTextFrame *src);
int64_t simrt_text_getint(SimrtTextFrame *frame);
void simrt_text_putint(SimrtTextFrame *frame, int64_t value);
int64_t simrt_text_getfrac(SimrtTextFrame *frame);
void simrt_text_putfrac(SimrtTextFrame *frame, int64_t value, int64_t places);
double simrt_text_getreal(SimrtTextFrame *frame);
void simrt_text_putfix(SimrtTextFrame *frame, double value, int64_t places);
void simrt_text_putreal_ex(SimrtTextFrame *frame, double value, int64_t n, int64_t exp_digits);
void simrt_text_putreal(SimrtTextFrame *frame, double value, int64_t n);
void simrt_text_upcase(SimrtTextFrame *frame);
void simrt_text_lowcase(SimrtTextFrame *frame);
int64_t simrt_error_text(SimrtTextFrame *frame);

/* --- Objects: gc-roots: none (single alloc) ---------------------------- */

SIMRT_MUST_USE simrt_gc_ptr simrt_object_alloc(int64_t size, int64_t class_id);
int64_t simrt_object_class_id(simrt_gc_ptr obj);
int64_t simrt_object_class_id_safe(simrt_gc_ptr obj);
int64_t simrt_object_load_i64(simrt_gc_ptr obj, int64_t offset);
void simrt_object_store_i64(simrt_gc_ptr obj, int64_t offset, int64_t value);

/* --- BASICIO: file objects are C-runtime GC roots ---------------------- */

SIMRT_MUST_USE simrt_gc_ptr simrt_sysin(void);
SIMRT_MUST_USE simrt_gc_ptr simrt_sysout(void);
void simrt_basicio_register_file(simrt_gc_ptr object, SimrtTextFrame *path_frame, int64_t mode);
int32_t simrt_basicio_open(simrt_gc_ptr object, SimrtTextFrame *fileimage);
int32_t simrt_basicio_open_byte(simrt_gc_ptr object);
int32_t simrt_basicio_close(simrt_gc_ptr object);
int32_t simrt_basicio_isopen(simrt_gc_ptr object);
void simrt_basicio_outtext(simrt_gc_ptr object, SimrtTextFrame *text);
void simrt_basicio_outchar(simrt_gc_ptr object, int64_t ch);
void simrt_basicio_outimage(simrt_gc_ptr object);
void simrt_basicio_breakoutimage(simrt_gc_ptr object);
void simrt_basicio_inimage(simrt_gc_ptr object);
void simrt_basicio_locate(simrt_gc_ptr object, int64_t i);
int64_t simrt_basicio_location(simrt_gc_ptr object);
int64_t simrt_basicio_lastloc(simrt_gc_ptr object);
void simrt_basicio_outreal_ex(
    simrt_gc_ptr object, double value, int64_t n, int64_t w, int64_t exp_digits
);
void simrt_basicio_outreal(simrt_gc_ptr object, double value, int64_t n, int64_t w);
void simrt_basicio_outfix(simrt_gc_ptr object, double value, int64_t n, int64_t w);
void simrt_basicio_outfrac(simrt_gc_ptr object, int64_t value, int64_t n, int64_t w);
void simrt_basicio_outint(simrt_gc_ptr object, int64_t value, int64_t w);
int64_t simrt_basicio_line(simrt_gc_ptr object);
int32_t simrt_basicio_setaccess(simrt_gc_ptr object, SimrtTextFrame *mode_frame);
void simrt_basicio_eject(simrt_gc_ptr object, int64_t n);
int64_t simrt_basicio_linesperpage(simrt_gc_ptr object, int64_t n);
int32_t simrt_basicio_inrecord(simrt_gc_ptr object);
SIMRT_MUST_USE SimrtTextFrame *simrt_basicio_filename(simrt_gc_ptr object);
SIMRT_MUST_USE SimrtTextFrame *simrt_basicio_image(simrt_gc_ptr object);
void simrt_basicio_set_image(simrt_gc_ptr object, SimrtTextFrame *text);
void simrt_basicio_setpos(simrt_gc_ptr object, int64_t i);
int64_t simrt_basicio_pos(simrt_gc_ptr object);
int64_t simrt_basicio_length(simrt_gc_ptr object);
int32_t simrt_basicio_inchar(simrt_gc_ptr object);
int32_t simrt_basicio_lastitem(simrt_gc_ptr object);
int64_t simrt_basicio_inint(simrt_gc_ptr object);
double simrt_basicio_inreal(simrt_gc_ptr object);
int64_t simrt_basicio_infrac(simrt_gc_ptr object);
SIMRT_MUST_USE SimrtTextFrame *simrt_basicio_intext(simrt_gc_ptr object, int64_t w);
int32_t simrt_basicio_endfile(simrt_gc_ptr object);
int32_t simrt_basicio_inbyte(simrt_gc_ptr object);
void simrt_basicio_outbyte(simrt_gc_ptr object, int64_t x);
void simrt_terminate_program(void);
int simrt_file_exists(SimrtTextFrame *path_frame);
SIMRT_MUST_USE SimrtTextFrame *simrt_file_read(SimrtTextFrame *path_frame);
void simrt_file_write(SimrtTextFrame *path_frame, SimrtTextFrame *contents_frame);

/* Host table (`runtime/embed.c`). Library Host thunks call resolve. */
void *simrt_host_resolve(const char *name);
void simrt_register_export(const char *name, void *fn, int32_t sig);

/* --- Simulation / SIMSET ---------------------------------------------- */

void simrt_sim_begin(void);
void simrt_sim_end(void);
double simrt_sim_time(void);
int64_t simrt_sim_is_active(void);
int64_t simrt_sim_is_main_current(void);
simrt_gc_ptr simrt_sim_current(void);
simrt_gc_ptr simrt_sim_main(void);
int64_t simrt_sim_idle(simrt_gc_ptr process);
int64_t simrt_sim_terminated(simrt_gc_ptr process);
double simrt_sim_evtime(simrt_gc_ptr process);
simrt_gc_ptr simrt_sim_nextev(simrt_gc_ptr process);
void simrt_sim_hold(double dt);
void simrt_sim_activate_direct(simrt_gc_ptr process);
void simrt_sim_activate_timed(
    simrt_gc_ptr process, double t, int64_t mode, int64_t prior, int64_t reac
);
void simrt_sim_activate_relative(simrt_gc_ptr process, simrt_gc_ptr other, int64_t before);
void simrt_sim_transfer_to_head(void);
void simrt_sim_terminate_current(simrt_gc_ptr process);
void simrt_sim_passivate(void);
void simrt_sim_cancel(simrt_gc_ptr process);
void simrt_sim_finish_main(void);
int64_t simrt_sim_has_current(void);
void simrt_simset_set_head_class_id(int64_t class_id);
void simrt_simset_init_head(simrt_gc_ptr head);
void simrt_simset_out(simrt_gc_ptr x);
void simrt_simset_precede(simrt_gc_ptr x, simrt_gc_ptr ptr);
void simrt_simset_follow(simrt_gc_ptr x, simrt_gc_ptr ptr);
void simrt_simset_into(simrt_gc_ptr x, simrt_gc_ptr head);
simrt_gc_ptr simrt_simset_suc(simrt_gc_ptr x);
simrt_gc_ptr simrt_simset_pred(simrt_gc_ptr x);
simrt_gc_ptr simrt_simset_first(simrt_gc_ptr head);
simrt_gc_ptr simrt_simset_last(simrt_gc_ptr head);
int64_t simrt_simset_empty(simrt_gc_ptr head);
int64_t simrt_simset_cardinal(simrt_gc_ptr head);

#ifdef __cplusplus
}
#endif

#endif /* SIMRT_RUNTIME_H */
