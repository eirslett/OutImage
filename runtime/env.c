#include <ctype.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

#include "internal.h"

/* ENVIRONMENT CURRENTDECIMALMARK / CURRENTLOWTEN (Standard §9.2). */
char g_decimal_mark = '.';
char g_lowten = '&';

int64_t simrt_decimalmark(int64_t c) {
    char previous = g_decimal_mark;
    if (c == '.' || c == ',') {
        g_decimal_mark = (char)c;
    } else {
        simrt_error("decimalmark argument must be '.' or ','");
    }
    return (int64_t)(unsigned char)previous;
}

int64_t simrt_lowten(int64_t c) {
    char previous = g_lowten;
    unsigned code = (unsigned)(unsigned char)c;
    if ((c >= '0' && c <= '9') || c == '+' || c == '-' || c == '.' || c == ','
        || code < 32 || code == 127 || code > 127) {
        simrt_error("illegal lowten character");
    }
    g_lowten = (char)c;
    return (int64_t)(unsigned char)previous;
}

int64_t simrt_current_decimalmark(void) {
    return (int64_t)(unsigned char)g_decimal_mark;
}

int64_t simrt_current_lowten(void) {
    return (int64_t)(unsigned char)g_lowten;
}

/* ENVIRONMENT math helpers used by MIR (also available via libm). */
double simrt_sqrt(double x) {
    if (x < 0.0) {
        simrt_error("sqrt of negative argument");
    }
    return sqrt(x);
}

double simrt_sin(double x) { return sin(x); }
double simrt_cos(double x) { return cos(x); }
double simrt_tan(double x) { return tan(x); }
double simrt_ln(double x) {
    if (x <= 0.0) {
        simrt_error("ln of non-positive argument");
    }
    return log(x);
}
double simrt_exp(double x) { return exp(x); }
double simrt_arctan(double x) { return atan(x); }

double simrt_sinh(double x) { return sinh(x); }
double simrt_cosh(double x) { return cosh(x); }
double simrt_tanh(double x) { return tanh(x); }
double simrt_log10(double x) {
    if (x <= 0.0) {
        simrt_error("log10 domain error");
    }
    return log10(x);
}

int64_t simrt_digit(int64_t c) {
    return (c >= '0' && c <= '9') ? 1 : 0;
}

int64_t simrt_letter(int64_t c) {
    return ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z')) ? 1 : 0;
}

int64_t simrt_char(int64_t i) {
    if (i < 0 || i > 255) {
        simrt_error("char argument out of range");
    }
    return i;
}

int64_t simrt_isochar(int64_t i) {
    if (i < 0 || i > 255) {
        simrt_error("isochar argument out of range");
    }
    return i;
}

int64_t simrt_rank(int64_t c) {
    return (int64_t)(unsigned char)c;
}

int64_t simrt_isorank(int64_t c) {
    return (int64_t)(unsigned char)c;
}

int64_t simrt_max_int(int64_t a, int64_t b) {
    return a >= b ? a : b;
}

int64_t simrt_min_int(int64_t a, int64_t b) {
    return a <= b ? a : b;
}

double simrt_max_real(double a, double b) {
    return a >= b ? a : b;
}

double simrt_min_real(double a, double b) {
    return a <= b ? a : b;
}

double simrt_cotan(double x) {
    double t = tan(x);
    if (t == 0.0) {
        simrt_error("cotan of multiple of pi");
    }
    return 1.0 / t;
}

double simrt_arcsin(double x) {
    if (x < -1.0 || x > 1.0) {
        simrt_error("arcsin domain error");
    }
    return asin(x);
}

double simrt_arccos(double x) {
    if (x < -1.0 || x > 1.0) {
        simrt_error("arccos domain error");
    }
    return acos(x);
}

double simrt_arctan2(double y, double x) {
    if (y == 0.0 && x == 0.0) {
        simrt_error("arctan2(0,0) undefined");
    }
    return atan2(y, x);
}

double simrt_addepsilon(double x) {
    return nextafter(x, INFINITY);
}

double simrt_subepsilon(double x) {
    return nextafter(x, -INFINITY);
}

/* §9.9 pseudo-random stream (n=31, p=13): U := rem(U * 5^(2p+1), 2^n). */
enum { SIMRT_RT_STREAM_BITS = 31 };
static const int64_t SIMRT_RT_STREAM_MODULUS = 1LL << SIMRT_RT_STREAM_BITS;
static const int64_t SIMRT_RT_STREAM_MULTIPLIER = 7450580596923828125LL; /* 5^27 */

static int64_t simrt_entier(double r) {
    int64_t j = (int64_t)r;
    if ((double)j > r) {
        return j - 1;
    }
    return j;
}

static int64_t simrt_advance_stream(int64_t u) {
    __int128 product = (__int128)u * (__int128)SIMRT_RT_STREAM_MULTIPLIER;
    __int128 modulus = (__int128)SIMRT_RT_STREAM_MODULUS;
    __int128 q = product / modulus;
    return (int64_t)(product - q * modulus);
}

double simrt_basic_draw(int64_t *stream) {
    int antithetic = *stream < 0;
    int64_t seed = antithetic ? -(*stream) : *stream;
    int64_t next = simrt_advance_stream(seed);
    double val;
    *stream = antithetic ? -next : next;
    val = (double)next * ldexp(1.0, -SIMRT_RT_STREAM_BITS);
    if (antithetic) {
        return val != 0.0 ? 1.0 - val : 0.0;
    }
    return val;
}

int64_t simrt_draw(double a, int64_t *stream) {
    if (stream == NULL) {
        simrt_error("draw: null random stream");
    }
    if (a >= 1.0) {
        return 1;
    }
    if (a <= 0.0) {
        return 0;
    }
    return simrt_basic_draw(stream) < a ? 1 : 0;
}

int64_t simrt_randint(int64_t a, int64_t b, int64_t *stream) {
    if (stream == NULL) {
        simrt_error("randint: null random stream");
    }
    if (b < a) {
        simrt_error("randint: b < a");
    }
    {
        int64_t span = b - a + 1;
        double u = simrt_basic_draw(stream);
        return a + simrt_entier(u * (double)span);
    }
}

double simrt_uniform(double a, double b, int64_t *stream) {
    if (stream == NULL) {
        simrt_error("uniform: null random stream");
    }
    if (b < a) {
        simrt_error("uniform: b < a");
    }
    {
        double u = simrt_basic_draw(stream);
        return a + u * (b - a);
    }
}

/* Match Rust `environment::normal`: one basic drawing used for both ln and cos. */
double simrt_normal(double a, double b, int64_t *stream) {
    double u;
    double z;
    if (stream == NULL) {
        simrt_error("normal: null random stream");
    }
    u = simrt_basic_draw(stream);
    if (u == 0.0) {
        return a;
    }
    z = sqrt(-2.0 * log(u)) * cos(2.0 * 3.14159265358979323846 * u);
    return a + b * z;
}

double simrt_negexp(double a, int64_t *stream) {
    double u;
    if (stream == NULL) {
        simrt_error("negexp: null random stream");
    }
    if (a <= 0.0) {
        simrt_error("negexp: non-positive rate");
    }
    u = simrt_basic_draw(stream);
    if (u == 0.0) {
        return INFINITY;
    }
    return -log(u) / a;
}

int64_t simrt_poisson(double a, int64_t *stream) {
    if (stream == NULL) {
        simrt_error("poisson: null random stream");
    }
    if (a <= 0.0) {
        return 0;
    }
    /* Match Rust POISSON_NORMAL_THRESHOLD = 20.0 */
    if (a > 20.0) {
        double sample = simrt_normal(a, sqrt(a), stream);
        int64_t n = simrt_entier(sample + 0.5);
        return n < 0 ? 0 : n;
    }
    {
        double threshold = exp(-a);
        double product = 1.0;
        int64_t n = 0;
        for (;;) {
            product *= simrt_basic_draw(stream);
            if (product < threshold) {
                return n;
            }
            n += 1;
        }
    }
}

double simrt_erlang(double a, double b, int64_t *stream) {
    int64_t c;
    double sum;
    int64_t i;
    double u;
    if (stream == NULL) {
        simrt_error("erlang: null random stream");
    }
    if (a <= 0.0 || b <= 0.0) {
        simrt_error("erlang: parameters must be positive");
    }
    c = simrt_entier(b);
    sum = 0.0;
    for (i = 0; i < c; i++) {
        u = simrt_basic_draw(stream);
        if (u == 0.0) {
            return INFINITY;
        }
        sum += log(u);
    }
    if ((double)c == b && c > 0) {
        return -sum / (a * b);
    }
    u = simrt_basic_draw(stream);
    if (u == 0.0) {
        return INFINITY;
    }
    sum += (b - (double)c) * log(u);
    return -sum / (a * b);
}

static int g_cputime_started = 0;
static clock_t g_cputime_origin;

double simrt_cputime(void) {
    if (!g_cputime_started) {
        g_cputime_origin = clock();
        g_cputime_started = 1;
        return 0.0;
    }
    return (double)(clock() - g_cputime_origin) / (double)CLOCKS_PER_SEC;
}

double simrt_clocktime(void) {
    time_t now = time(NULL);
    struct tm local;
#if defined(_WIN32)
    localtime_s(&local, &now);
#else
    localtime_r(&now, &local);
#endif
    return (double)local.tm_hour * 3600.0
        + (double)local.tm_min * 60.0
        + (double)local.tm_sec;
}

int64_t simrt_rem(int64_t i, int64_t j) {
    if (j == 0) {
        simrt_error("rem with zero divisor");
    }
    /* Truncating remainder: i - (i / j) * j (C toward-zero division). */
    return i - (i / j) * j;
}

int64_t simrt_mod(int64_t i, int64_t j) {
    int64_t res;
    int64_t s_res;
    int64_t s_j;
    if (j == 0) {
        simrt_error("mod with zero divisor");
    }
    /* Simula mathematical modulo (§9.1): rem adjusted so sign matches divisor. */
    res = i - (i / j) * j;
    if (res == 0) {
        return 0;
    }
    s_res = res > 0 ? 1 : -1;
    s_j = j > 0 ? 1 : -1;
    if (s_res != s_j) {
        return res + j;
    }
    return res;
}

int64_t simrt_sign(double r) {
    if (r > 0.0) {
        return 1;
    }
    if (r < 0.0) {
        return -1;
    }
    return 0;
}

double simrt_abs_real(double r) {
    return fabs(r);
}

int64_t simrt_abs_int(int64_t i) {
    if (i == INT64_MIN) {
        simrt_error("abs integer overflow");
    }
    return i < 0 ? -i : i;
}

/* Simula §3.5.4: x**y is undefined for x < 0 unless y is an integer
 * (then repeated multiplication applies); also undefined for 0**non-positive.
 * For x > 0 with a non-integer exponent, use exp(y*ln(x)) so results match
 * the DosTestBatch simtst06 `rpower` identity. Integer exponents keep `pow`
 * for multiplicative accuracy (simtst28). */
double simrt_f64_pow(double base, double exponent) {
    if (base == 0.0 && exponent <= 0.0) {
        simrt_error("exponentiation undefined");
    }
    if (base < 0.0) {
        if (exponent != trunc(exponent)) {
            simrt_error("exponentiation undefined");
        }
        return pow(base, exponent);
    }
    if (base > 0.0 && exponent != trunc(exponent)) {
        return exp(exponent * log(base));
    }
    return pow(base, exponent);
}
