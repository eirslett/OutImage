#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#include <io.h>
#include <windows.h>
#else
#include <unistd.h>
#endif

#include "internal.h"

/* Implementation-defined SYSIN / SYSOUT image lengths (Simula §10 intro). */
#ifndef SIMRT_SYSIN_LINELENGTH
#define SIMRT_SYSIN_LINELENGTH 80
#endif
#ifndef SIMRT_SYSOUT_LINELENGTH
#define SIMRT_SYSOUT_LINELENGTH 132
#endif

static unsigned char line[4096];
static size_t line_len;
/* 1-based SysOut image position (Standard BASICIO). Writing starts at `line_pos`. */
static size_t line_pos = 1;

/* SysIn image buffer (free InImage / InChar / Endfile). */
static unsigned char sysin_line[4096];
static size_t sysin_len;
static size_t sysin_pos = 1;
static int sysin_endfile = 0;
/* Length of the SysIn image once the program has given it one of its own
 * (`sysin.image :- sysin.image.sub(1,5)`): 10.4.2 transfers the external record
 * *into* the image, so the image keeps its own length rather than taking the
 * record's. Zero while the image is still the free line buffer. */
static size_t sysin_image_len = 0;
static int simrt_image_bufs_inited = 0;

/* §10: SYSIN.open(blanks(INPUT_LINELENGTH)). SYSOUT free buffer starts empty and
 * grows with Out*; the BASICIO sysout object still tracks a blank image. */
static void simrt_image_buffers_ensure_init(void) {
    if (simrt_image_bufs_inited) {
        return;
    }
    memset(sysin_line, ' ', SIMRT_SYSIN_LINELENGTH);
    sysin_len = SIMRT_SYSIN_LINELENGTH;
    sysin_pos = SIMRT_SYSIN_LINELENGTH + 1;
    line_len = 0;
    line_pos = 1;
    simrt_image_bufs_inited = 1;
}

static void simrt_image_reset(unsigned char *buf, size_t *len, size_t *pos) {
    (void)buf;
    *len = 0;
    *pos = 1;
}

/* Write `text` at 1-based `*pos`, growing content; advance `*pos`. */
static void simrt_image_out_text(
    unsigned char *buf,
    size_t capacity,
    size_t *len,
    size_t *pos,
    const unsigned char *text,
    size_t length
) {
    size_t start;
    size_t i;
    if (text == NULL || length == 0) {
        return;
    }
    start = *pos > 0 ? *pos - 1 : 0;
    if (start > *len) {
        size_t pad = start - *len;
        if (*len + pad > capacity) {
            pad = capacity > *len ? capacity - *len : 0;
        }
        memset(buf + *len, ' ', pad);
        *len += pad;
        start = *len;
    }
    for (i = 0; i < length; i++) {
        size_t at = start + i;
        if (at >= capacity) {
            break;
        }
        buf[at] = text[i];
        if (at >= *len) {
            *len = at + 1;
        }
    }
    *pos = start + length + 1;
    if (*pos > capacity + 1) {
        *pos = capacity + 1;
    }
}

static void simrt_write_bytes(const unsigned char *data, size_t length) {
#ifdef _WIN32
    HANDLE stdout_handle = GetStdHandle(STD_OUTPUT_HANDLE);
    if (stdout_handle != INVALID_HANDLE_VALUE && length > 0) {
        DWORD written = 0;
        WriteFile(stdout_handle, data, (DWORD)length, &written, NULL);
    }
#else
    if (length > 0) {
        write(1, data, length);
    }
#endif
}

static void simrt_write_newline(void) {
#ifdef _WIN32
    HANDLE stdout_handle = GetStdHandle(STD_OUTPUT_HANDLE);
    if (stdout_handle != INVALID_HANDLE_VALUE) {
        DWORD written = 0;
        WriteFile(stdout_handle, "\n", 1, &written, NULL);
    }
#else
    write(1, "\n", 1);
#endif
}


void simrt_out_text(const unsigned char *text, size_t length) {
    simrt_image_buffers_ensure_init();
    simrt_image_out_text(line, sizeof(line), &line_len, &line_pos, text, length);
}

void simrt_out_char(int64_t ch) {
    unsigned char buf[4];
    size_t n = 0;
    uint32_t cp = (uint32_t)ch;
    /* Encode Unicode codepoint as UTF-8 (MVP; ASCII stays one byte). */
    if (cp <= 0x7Fu) {
        buf[0] = (unsigned char)cp;
        n = 1;
    } else if (cp <= 0x7FFu) {
        buf[0] = (unsigned char)(0xC0u | (cp >> 6));
        buf[1] = (unsigned char)(0x80u | (cp & 0x3Fu));
        n = 2;
    } else if (cp <= 0xFFFFu) {
        buf[0] = (unsigned char)(0xE0u | (cp >> 12));
        buf[1] = (unsigned char)(0x80u | ((cp >> 6) & 0x3Fu));
        buf[2] = (unsigned char)(0x80u | (cp & 0x3Fu));
        n = 3;
    } else {
        buf[0] = (unsigned char)(0xF0u | (cp >> 18));
        buf[1] = (unsigned char)(0x80u | ((cp >> 12) & 0x3Fu));
        buf[2] = (unsigned char)(0x80u | ((cp >> 6) & 0x3Fu));
        buf[3] = (unsigned char)(0x80u | (cp & 0x3Fu));
        n = 4;
    }
    simrt_out_text(buf, n);
}

void simrt_out_image(void) {
    simrt_image_buffers_ensure_init();
    /* OutImage emits the full image content (not only break payload). */
    simrt_write_bytes(line, line_len);
    simrt_write_newline();
    simrt_image_reset(line, &line_len, &line_pos);
}

void simrt_break_out_image(void) {
    size_t break_len = line_pos > 0 ? line_pos - 1 : 0;
    if (break_len > line_len) {
        break_len = line_len;
    }
    simrt_write_bytes(line, break_len);
    simrt_write_newline();
    simrt_image_reset(line, &line_len, &line_pos);
}

void simrt_in_image(void) {
    size_t record;
    if (fgets((char *)sysin_line, (int)sizeof(sysin_line), stdin) == NULL) {
        sysin_endfile = 1;
        simrt_image_reset(sysin_line, &sysin_len, &sysin_pos);
        return;
    }
    record = strlen((const char *)sysin_line);
    while (record > 0 && (sysin_line[record - 1] == '\n' || sysin_line[record - 1] == '\r')) {
        record--;
    }
    if (sysin_image_len > 0) {
        /* "the record is blank-padded / truncated into the image". */
        if (record < sysin_image_len) {
            memset(sysin_line + record, ' ', sysin_image_len - record);
        }
        sysin_len = sysin_image_len;
    } else {
        sysin_len = record;
    }
    sysin_pos = 1;
    sysin_endfile = 0;
}

int32_t simrt_in_char(void) {
    size_t idx;
    unsigned char ch;
    simrt_image_buffers_ensure_init();
    if (sysin_endfile) {
        simrt_error("InChar: end of file");
    }
    /* 10.4.3: `if not more then inimage` — an image the program supplied is no
     * exception, it is simply refilled from the file once exhausted. */
    if (sysin_pos > sysin_len) {
        simrt_in_image();
    }
    if (sysin_endfile && sysin_pos > sysin_len) {
        simrt_error("InChar: end of file");
    }
    idx = sysin_pos > 0 ? sysin_pos - 1 : 0;
    if (idx >= sysin_len) {
        simrt_error("InChar: no more characters in image (call InImage first)");
    }
    ch = sysin_line[idx];
    sysin_pos++;
    return (int32_t)ch;
}

int32_t simrt_endfile(void) {
    return sysin_endfile ? 1 : 0;
}

/* BASICIO-ish `InLine` MVP: read one line from stdin (up to 255 bytes + NUL),
 * strip a trailing `\n` and preceding `\r`, and return a text frame. Matches
 * the wasm `CallInLine` 256-byte buffer MVP and the interpreter's strip. */
SimrtTextFrame *simrt_in_line(void) {
    unsigned char buf[256];
    if (fgets((char *)buf, (int)sizeof(buf), stdin) == NULL) {
        return simrt_text_notext();
    }
    size_t len = strlen((const char *)buf);
    while (len > 0 && (buf[len - 1] == '\n' || buf[len - 1] == '\r')) {
        len--;
    }
    return simrt_text_from_literal(buf, len);
}

void simrt_out_int(int64_t value, int64_t w) {
    char digits[32];
    char buffer[64];
    int n = snprintf(digits, sizeof(digits), "%lld", (long long)value);
    size_t dig_len;
    size_t width;
    size_t i;
    if (n < 0) {
        return;
    }
    dig_len = (size_t)n < sizeof(digits) ? (size_t)n : sizeof(digits) - 1;
    if (w == 0) {
        simrt_out_text((const unsigned char *)digits, dig_len);
        return;
    }
    width = (size_t)(w < 0 ? -w : w);
    if (width >= sizeof(buffer)) {
        width = sizeof(buffer) - 1;
    }
    /* Standard §10.5.8 / text.putint: overlong items are asterisk-filled. */
    if (dig_len > width) {
        memset(buffer, '*', width);
        simrt_out_text((const unsigned char *)buffer, width);
        return;
    }
    if (w > 0) {
        for (i = 0; i < width - dig_len; i++) {
            buffer[i] = ' ';
        }
        memcpy(buffer + (width - dig_len), digits, dig_len);
    } else {
        memcpy(buffer, digits, dig_len);
        for (i = dig_len; i < width; i++) {
            buffer[i] = ' ';
        }
    }
    simrt_out_text((const unsigned char *)buffer, width);
}

void simrt_out_real_ex(double value, int64_t n, int64_t w, int64_t exp_digits) {
    char item[128];
    unsigned char field[SIMRT_NUMERIC_FIELD_MAX];
    size_t item_len =
        simrt_format_real_ex(item, sizeof(item), value, n, (int)exp_digits);
    size_t field_len;
    simrt_pad_numeric_field(field, &field_len, item, item_len, w);
    simrt_out_text(field, field_len);
}

void simrt_out_real(double value, int64_t n, int64_t w) {
    simrt_out_real_ex(value, n, w, 2);
}

void simrt_out_fix(double value, int64_t n, int64_t w) {
    char item[128];
    unsigned char field[SIMRT_NUMERIC_FIELD_MAX];
    size_t item_len = simrt_format_fix(item, sizeof(item), value, n);
    size_t field_len;
    simrt_pad_numeric_field(field, &field_len, item, item_len, w);
    simrt_out_text(field, field_len);
}

void simrt_out_frac(int64_t value, int64_t n, int64_t w) {
    char item[128];
    unsigned char field[SIMRT_NUMERIC_FIELD_MAX];
    size_t item_len = simrt_format_frac(item, sizeof(item), value, n);
    size_t field_len;
    simrt_pad_numeric_field(field, &field_len, item, item_len, w);
    simrt_out_text(field, field_len);
}

/* BASICIO file registry keyed by object pointer (native only). */
enum { SIMRT_BASICIO_MAX_FILES = 64 }; /* §0.5.2; `too many open files` */

typedef struct {
    void *object;
    char *path;
    /* 0=infile, 1=outfile, 2=inbytefile, 3=outbytefile,
     * 4=directfile, 5=directbytefile, 6=printfile. */
    int mode;
    int open;
    int is_terminal;
    FILE *fp;
    unsigned char image[4096];
    size_t image_len;
    size_t image_pos;
    int endfile;
    /* DirectFile in-memory images (§10.6); index 0 = location 1. */
    char **direct_lines;
    size_t direct_count;
    int64_t loc;
    int64_t maxloc;
    /* PrintFile pagination (§10.7). */
    int64_t line;
    int64_t page;
    int64_t spacing;
    int64_t lines_per_page;
    int64_t default_lines_per_page;
    /* §10.1.1 access modes (subset used by open). */
    int append;
    /* 0=anycreate, 1=create, 2=nocreate */
    int create_mode;
    /* Closed and reusable: the slot still answers `isopen` / `filename` / a
     * reopen for its object, but may be recycled when the registry is full. */
    int retired;
} SimrtBasicioFile;

static SimrtBasicioFile g_basicio_files[SIMRT_BASICIO_MAX_FILES];
static size_t g_basicio_file_count;
static void *g_sysin_obj;
static void *g_sysout_obj;

static SimrtBasicioFile *basicio_find(void *object) {
    size_t i;
    if (object == NULL) {
        return NULL;
    }
    for (i = 0; i < g_basicio_file_count; i++) {
        if (g_basicio_files[i].object == object) {
            return &g_basicio_files[i];
        }
    }
    return NULL;
}

static SimrtBasicioFile *basicio_require(void *object, const char *what) {
    SimrtBasicioFile *file = basicio_find(object);
    if (file == NULL) {
        char buf[96];
        snprintf(buf, sizeof(buf), "%s: unknown BASICIO file object", what);
        simrt_error(buf);
    }
    return file;
}

static void basicio_direct_free(SimrtBasicioFile *file);

/* Registry slots are reclaimed rather than leaked monotonically.
 * A
 * closed file keeps its slot so `isopen`, `filename`, and a later reopen still
 * resolve for that object, at parity with the interpreter's file table, but is
 * marked `retired` so a new registration can take the slot over once the
 * fixed-size registry is full. */
static void basicio_slot_release(SimrtBasicioFile *slot) {
    basicio_direct_free(slot);
    if (slot->fp != NULL) {
        fclose(slot->fp);
        slot->fp = NULL;
    }
    free(slot->path);
    slot->path = NULL;
    slot->object = NULL;
    slot->open = 0;
    slot->retired = 0;
}

static SimrtBasicioFile *basicio_claim_slot(const char *what) {
    size_t i;
    for (i = 0; i < g_basicio_file_count; i++) {
        if (g_basicio_files[i].object == NULL) {
            return &g_basicio_files[i];
        }
    }
    if (g_basicio_file_count < SIMRT_BASICIO_MAX_FILES) {
        return &g_basicio_files[g_basicio_file_count++];
    }
    for (i = 0; i < g_basicio_file_count; i++) {
        if (g_basicio_files[i].retired) {
            basicio_slot_release(&g_basicio_files[i]);
            return &g_basicio_files[i];
        }
    }
    {
        char buf[96];
        snprintf(buf, sizeof(buf), "%s: too many open files", what);
        simrt_error(buf);
    }
    return NULL;
}

static void basicio_direct_free(SimrtBasicioFile *file) {
    size_t i;
    if (file->direct_lines == NULL) {
        return;
    }
    for (i = 0; i < file->direct_count; i++) {
        free(file->direct_lines[i]);
        file->direct_lines[i] = NULL;
    }
    free(file->direct_lines);
    file->direct_lines = NULL;
    file->direct_count = 0;
}

static int basicio_direct_ensure(SimrtBasicioFile *file, size_t need) {
    char **grown;
    size_t i;
    if (need <= file->direct_count) {
        return 1;
    }
    grown = (char **)simrt_host_realloc_n(file->direct_lines, need, sizeof(char *));
    if (grown == NULL) {
        return 0;
    }
    for (i = file->direct_count; i < need; i++) {
        grown[i] = NULL;
    }
    file->direct_lines = grown;
    file->direct_count = need;
    return 1;
}

static int basicio_direct_load(SimrtBasicioFile *file) {
    FILE *fp;
    char line[4096];
    size_t n;
    size_t idx = 0;
    basicio_direct_free(file);
    fp = fopen(file->path, "r");
    if (fp == NULL) {
        return 1; /* empty new file */
    }
    while (fgets(line, (int)sizeof(line), fp) != NULL) {
        n = strlen(line);
        while (n > 0 && (line[n - 1] == '\n' || line[n - 1] == '\r')) {
            n--;
        }
        line[n] = '\0';
        if (!basicio_direct_ensure(file, idx + 1)) {
            fclose(fp);
            return 0;
        }
        file->direct_lines[idx] = (char *)simrt_host_malloc_sum(n, 1);
        if (file->direct_lines[idx] == NULL) {
            fclose(fp);
            return 0;
        }
        memcpy(file->direct_lines[idx], line, n + 1);
        idx++;
    }
    fclose(fp);
    return 1;
}

static int basicio_direct_persist(SimrtBasicioFile *file) {
    FILE *fp;
    size_t i;
    if (file->path == NULL) {
        return 0;
    }
    fp = fopen(file->path, "w");
    if (fp == NULL) {
        return 0;
    }
    for (i = 0; i < file->direct_count; i++) {
        const char *line = file->direct_lines[i] != NULL ? file->direct_lines[i] : "";
        fputs(line, fp);
        fputc('\n', fp);
    }
    fclose(fp);
    return 1;
}

void simrt_basicio_outimage(void *object);
void simrt_basicio_inimage(void *object);
void simrt_basicio_outchar(void *object, int64_t ch);

void simrt_basicio_register_file(void *object, SimrtTextFrame *path_frame, int64_t mode) {
    SimrtBasicioFile *slot;
    size_t path_len;
    const unsigned char *path_ptr;
    if (object == NULL) {
        simrt_error("basicio register: none object");
    }
    if (basicio_find(object) != NULL) {
        return;
    }
    slot = basicio_claim_slot("basicio register");
    memset(slot, 0, sizeof(*slot));
    slot->object = object;
    slot->mode = (int)mode;
    slot->image_pos = 1;
    slot->line = 1;
    slot->spacing = 1;
    slot->lines_per_page = 60;
    slot->default_lines_per_page = 60;
    slot->append = 0;
    slot->create_mode = 0; /* anycreate */
    path_len = simrt_text_content_length(path_frame);
    path_ptr = simrt_text_content_ptr(path_frame);
    slot->path = (char *)simrt_host_malloc_sum(path_len, 1);
    if (slot->path == NULL) {
        simrt_text_oom();
    }
    if (path_len > 0 && path_ptr != NULL) {
        memcpy(slot->path, path_ptr, path_len);
    }
    slot->path[path_len] = '\0';
}

/* Implementation-defined SYSIN / SYSOUT image lengths are defined near the
 * top of this file (SIMRT_SYSIN_LINELENGTH / SIMRT_SYSOUT_LINELENGTH). */

void *simrt_sysin(void) {
    simrt_image_buffers_ensure_init();
    if (g_sysin_obj == NULL) {
        SimrtBasicioFile *slot;
        g_sysin_obj = simrt_object_alloc((int64_t)sizeof(int64_t), /*class_id*/ 0);
        slot = basicio_claim_slot("sysin");
        memset(slot, 0, sizeof(*slot));
        slot->object = g_sysin_obj;
        slot->mode = 0;
        slot->open = 1;
        slot->is_terminal = 1;
        slot->path = (char *)simrt_text_host_alloc(8);
        memcpy(slot->path, "<SYSIN>", 8);
        /* Mirror the free SYSIN image into the file slot for non-terminal paths. */
        slot->image_len = SIMRT_SYSIN_LINELENGTH;
        memset(slot->image, ' ', slot->image_len);
        slot->image_pos = slot->image_len + 1;
    }
    return g_sysin_obj;
}

void *simrt_sysout(void) {
    simrt_image_buffers_ensure_init();
    if (g_sysout_obj == NULL) {
        SimrtBasicioFile *slot;
        g_sysout_obj = simrt_object_alloc((int64_t)sizeof(int64_t), /*class_id*/ 0);
        slot = basicio_claim_slot("sysout");
        memset(slot, 0, sizeof(*slot));
        slot->object = g_sysout_obj;
        slot->mode = 1;
        slot->open = 1;
        slot->is_terminal = 1;
        slot->path = (char *)simrt_text_host_alloc(9);
        memcpy(slot->path, "<SYSOUT>", 9);
        slot->image_len = SIMRT_SYSOUT_LINELENGTH;
        memset(slot->image, ' ', slot->image_len);
        slot->image_pos = 1;
        slot->line = 1;
    }
    return g_sysout_obj;
}

int32_t simrt_basicio_open(void *object, SimrtTextFrame *fileimage) {
    SimrtBasicioFile *file = basicio_require(object, "open");
    size_t image_len;
    /* Reopening un-retires the slot: the object is live again. */
    file->retired = 0;
    if (file->is_terminal) {
        file->open = 1;
        return 1;
    }
    if (file->open) {
        return 0;
    }
    if (file->path == NULL || file->path[0] == '\0') {
        return 0;
    }
    /* Bytefiles: image open is invalid — use open_byte. */
    if (file->mode == 2 || file->mode == 3 || file->mode == 5) {
        simrt_error("open: bytefile requires parameterless open");
    }
    image_len = simrt_text_content_length(fileimage);
    if (image_len == 0) {
        simrt_error("open: fileimage is notext");
    }
    if (image_len > sizeof(file->image)) {
        image_len = sizeof(file->image);
    }
    if (file->mode == 4) {
        /* DirectFile: in-memory images, optionally seeded from path. */
        if (!basicio_direct_load(file)) {
            return 0;
        }
        memset(file->image, ' ', image_len);
        file->image_len = image_len;
        file->image_pos = 1;
        file->loc = 1;
        file->maxloc = INT64_MAX - 1;
        file->open = 1;
        file->endfile = 0;
        return 1;
    }
    if (file->mode == 0) {
        /* Binary so Windows does not eat `\r` before inimage strips it; the
         * image path still treats the file as a stream of characters. */
        file->fp = fopen(file->path, "rb");
        if (file->fp == NULL) {
            return 0;
        }
    } else {
        /* OutFile / PrintFile: honour create/append access modes. */
        int exists = 0;
        FILE *probe = fopen(file->path, "r");
        if (probe != NULL) {
            exists = 1;
            fclose(probe);
        }
        if (file->create_mode == 1 && exists) {
            return 0; /* CREATE: fail if file already exists */
        }
        if (file->create_mode == 2 && !exists) {
            return 0; /* NOCREATE: fail if missing */
        }
        if (file->append) {
            file->fp = fopen(file->path, "a");
        } else {
            file->fp = fopen(file->path, "w");
        }
        if (file->fp == NULL) {
            return 0;
        }
    }
    memset(file->image, ' ', image_len);
    file->image_len = image_len;
    /* InFile open: setpos(length+1); OutFile: setpos(1). */
    file->image_pos = file->mode == 0 ? image_len + 1 : 1;
    file->open = 1;
    file->endfile = 0;
    file->line = 1;
    file->spacing = 1;
    if (file->lines_per_page <= 0) {
        file->lines_per_page = file->default_lines_per_page > 0
            ? file->default_lines_per_page
            : 60;
    }
    if (file->mode == 6) {
        /* PrintFile open: eject(1). */
        file->page = 0;
        file->line = 1;
    }
    return 1;
}

int32_t simrt_basicio_open_byte(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "open");
    file->retired = 0;
    if (file->is_terminal) {
        file->open = 1;
        return 1;
    }
    if (file->open) {
        return 0;
    }
    if (file->path == NULL || file->path[0] == '\0') {
        return 0;
    }
    if (file->mode == 2) {
        file->fp = fopen(file->path, "rb");
        if (file->fp == NULL) {
            return 0;
        }
    } else if (file->mode == 3) {
        file->fp = fopen(file->path, "wb");
        if (file->fp == NULL) {
            return 0;
        }
    } else if (file->mode == 5) {
        /* DirectByteFile (§10.11): in-memory byte store. Seed from path when
         * present; otherwise start empty (CREATE / missing file). Full byte
         * I/O is still MVP — open/close must succeed for corpus tests. */
        file->loc = 1;
        file->maxloc = INT64_MAX - 1;
        file->open = 1;
        file->endfile = 0;
        return 1;
    } else {
        simrt_error("open: image file requires fileimage");
    }
    file->open = 1;
    file->endfile = 0;
    return 1;
}

int32_t simrt_basicio_close(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "close");
    if (!file->open) {
        return 0;
    }
    if (file->is_terminal) {
        return 1;
    }
    if (file->mode == 4) {
        (void)basicio_direct_persist(file);
        basicio_direct_free(file);
        file->loc = 0;
        file->maxloc = 0;
        file->open = 0;
        file->endfile = 1;
        file->image_len = 0;
        file->image_pos = 1;
        file->retired = 1;
        return 1;
    }
    /* OutFile: if pos <> 1 then outimage. */
    if (file->mode != 0 && file->image_pos != 1 && file->fp != NULL) {
        fwrite(file->image, 1, file->image_len, file->fp);
        fputc('\n', file->fp);
        fflush(file->fp);
    }
    if (file->fp != NULL) {
        fclose(file->fp);
        file->fp = NULL;
    }
    file->open = 0;
    file->endfile = 1;
    file->image_len = 0;
    file->image_pos = 1;
    file->retired = 1;
    return 1;
}

int32_t simrt_basicio_isopen(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "isopen");
    return file->open ? 1 : 0;
}

void simrt_basicio_outtext(void *object, SimrtTextFrame *text) {
    SimrtBasicioFile *file = basicio_require(object, "outtext");
    size_t len;
    size_t i;
    const unsigned char *ptr;
    if (file->is_terminal) {
        len = simrt_text_content_length(text);
        ptr = simrt_text_content_ptr(text);
        simrt_out_text(ptr, len);
        return;
    }
    if (!file->open) {
        simrt_error("OutFile.outtext: file is not open");
    }
    /* InFile has no output procedures; ignore rather than clobber the image
     * (simtst96 under inspect InFile). */
    if (file->mode == 0 || file->mode == 2) {
        return;
    }
    len = simrt_text_content_length(text);
    ptr = simrt_text_content_ptr(text);
    if (file->image_pos > 1
        && (int64_t)len > (int64_t)file->image_len - (int64_t)file->image_pos + 1) {
        simrt_basicio_outimage(object);
    }
    for (i = 0; i < len; i++) {
        simrt_basicio_outchar(object, (int64_t)(ptr ? ptr[i] : 0));
    }
}

void simrt_basicio_outchar(void *object, int64_t ch) {
    unsigned char buf[4];
    size_t n = 0;
    uint32_t cp = (uint32_t)ch;
    SimrtBasicioFile *file = basicio_require(object, "outchar");
    if (cp <= 0x7Fu) {
        buf[0] = (unsigned char)cp;
        n = 1;
    } else if (cp <= 0x7FFu) {
        buf[0] = (unsigned char)(0xC0u | (cp >> 6));
        buf[1] = (unsigned char)(0x80u | (cp & 0x3Fu));
        n = 2;
    } else if (cp <= 0xFFFFu) {
        buf[0] = (unsigned char)(0xE0u | (cp >> 12));
        buf[1] = (unsigned char)(0x80u | ((cp >> 6) & 0x3Fu));
        buf[2] = (unsigned char)(0x80u | (cp & 0x3Fu));
        n = 3;
    } else {
        buf[0] = (unsigned char)(0xF0u | (cp >> 18));
        buf[1] = (unsigned char)(0x80u | ((cp >> 12) & 0x3Fu));
        buf[2] = (unsigned char)(0x80u | ((cp >> 6) & 0x3Fu));
        buf[3] = (unsigned char)(0x80u | (cp & 0x3Fu));
        n = 4;
    }
    if (file->is_terminal) {
        simrt_out_text(buf, n);
        return;
    }
    if (!file->open) {
        simrt_error("OutFile.outchar: file is not open");
    }
    if (file->image_pos > file->image_len) {
        simrt_basicio_outimage(object);
    }
    if (n == 1 && file->image_pos >= 1 && file->image_pos <= file->image_len) {
        file->image[file->image_pos - 1] = buf[0];
        file->image_pos++;
        return;
    }
    simrt_image_out_text(
        file->image, sizeof(file->image), &file->image_len, &file->image_pos, buf, n
    );
}

void simrt_basicio_outimage(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "outimage");
    size_t idx;
    char *copy;
    if (file->is_terminal) {
        simrt_out_image();
        file->line += 1;
        return;
    }
    if (!file->open) {
        simrt_error("OutFile.outimage: file is not open");
    }
    if (file->mode == 4) {
        if (file->loc < 1 || file->loc > file->maxloc) {
            simrt_error("outimage: file overflow");
        }
        idx = (size_t)(file->loc - 1);
        if (!basicio_direct_ensure(file, idx + 1)) {
            simrt_text_oom();
        }
        free(file->direct_lines[idx]);
        copy = (char *)simrt_host_malloc_sum(file->image_len, 1);
        if (copy == NULL) {
            simrt_text_oom();
        }
        memcpy(copy, file->image, file->image_len);
        copy[file->image_len] = '\0';
        file->direct_lines[idx] = copy;
        file->loc += 1;
        memset(file->image, ' ', file->image_len);
        file->image_pos = 1;
        return;
    }
    /* InFile / InByteFile: no writer. Match the interpreter — reset the image
     * only. Writing through a read-only `FILE*` on Windows advances/corrupts
     * the stream (simtst96: next command became `TBY DAL`). */
    if (file->mode == 0 || file->mode == 2) {
        memset(file->image, ' ', file->image_len);
        file->image_pos = 1;
        return;
    }
    if (file->fp == NULL) {
        simrt_error("OutFile.outimage: file is not open");
    }
    fwrite(file->image, 1, file->image_len, file->fp);
    fputc('\n', file->fp);
    fflush(file->fp);
    memset(file->image, ' ', file->image_len);
    file->image_pos = 1;
    if (file->mode == 6) {
        int64_t step = file->spacing > 0 ? file->spacing : 1;
        file->line += step;
    } else {
        file->line += 1;
    }
}

void simrt_basicio_breakoutimage(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "breakoutimage");
    size_t break_len;
    if (file->is_terminal) {
        simrt_break_out_image();
        file->line += 1;
        return;
    }
    if (!file->open || file->fp == NULL) {
        simrt_error("OutFile.breakoutimage: file is not open");
    }
    break_len = file->image_pos > 0 ? file->image_pos - 1 : 0;
    if (break_len > file->image_len) {
        break_len = file->image_len;
    }
    fwrite(file->image, 1, break_len, file->fp);
    fputc('\n', file->fp);
    fflush(file->fp);
    memset(file->image, ' ', file->image_len);
    file->image_pos = 1;
    file->line += 1;
}

void simrt_basicio_inimage(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "inimage");
    char line[4096];
    size_t n;
    size_t i;
    size_t idx;
    int64_t last;
    if (file->is_terminal) {
        simrt_in_image();
        return;
    }
    if (!file->open) {
        simrt_error("InFile.inimage: file is not open");
    }
    if (file->mode == 4) {
        last = (int64_t)file->direct_count;
        file->image_pos = 1;
        file->endfile = file->loc > last;
        memset(file->image, ' ', file->image_len);
        if (file->endfile) {
            if (file->image_len > 0) {
                file->image[0] = (unsigned char)0x19; /* EM */
            }
        } else {
            idx = (size_t)(file->loc - 1);
            if (idx < file->direct_count && file->direct_lines[idx] != NULL) {
                n = strlen(file->direct_lines[idx]);
                if (n > file->image_len) {
                    n = file->image_len;
                }
                for (i = 0; i < n; i++) {
                    file->image[i] = (unsigned char)file->direct_lines[idx][i];
                }
            } else {
                /* Unwritten image: NUL-filled, pos = length+1. */
                memset(file->image, 0, file->image_len);
                file->image_pos = file->image_len + 1;
                file->loc += 1;
                return;
            }
        }
        file->loc += 1;
        return;
    }
    if (file->fp == NULL) {
        simrt_error("InFile.inimage: file is not open");
    }
    if (file->endfile) {
        simrt_error("inimage: end of file");
    }
    if (fgets(line, (int)sizeof(line), file->fp) == NULL) {
        file->endfile = 1;
        /* EM (ISO rank 25) left-adjusted; remainder spaces. */
        memset(file->image, ' ', file->image_len);
        if (file->image_len > 0) {
            file->image[0] = (unsigned char)0x19;
        }
        file->image_pos = 1;
        return;
    }
    n = strlen(line);
    while (n > 0 && (line[n - 1] == '\n' || line[n - 1] == '\r')) {
        n--;
    }
    if (n > file->image_len) {
        simrt_error("inimage: image too short for external image");
    }
    memset(file->image, ' ', file->image_len);
    for (i = 0; i < n; i++) {
        file->image[i] = (unsigned char)line[i];
    }
    file->image_pos = 1;
    file->endfile = 0;
}

void simrt_basicio_locate(void *object, int64_t i) {
    SimrtBasicioFile *file = basicio_require(object, "locate");
    if (file->mode != 4) {
        simrt_error("locate: not a directfile");
    }
    if (!file->open) {
        simrt_error("locate: file is not open");
    }
    if (i < 1 || i > file->maxloc) {
        simrt_error("locate: parameter out of range");
    }
    file->loc = i;
}

int64_t simrt_basicio_location(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "location");
    if (file->mode != 4) {
        simrt_error("location: not a directfile");
    }
    return file->loc;
}

int64_t simrt_basicio_lastloc(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "lastloc");
    if (file->mode != 4 && file->mode != 5) {
        simrt_error("lastloc: not a direct file");
    }
    if (!file->open) {
        simrt_error("lastloc: file closed");
    }
    return (int64_t)file->direct_count;
}

/* `outreal`/`outfix`/`outfrac`/`line`/`image` bound to a BASICIO file
 * object: format via the same helpers as the free SYSOUT procedures, then
 * write through `outtext` so terminal vs. file-image handling stays uniform
 * with the rest of BASICIO. */
static void basicio_out_formatted(void *object, const char *item, size_t item_len) {
    SimrtTextFrame *frame = simrt_text_from_literal((const unsigned char *)item, item_len);
    simrt_basicio_outtext(object, frame);
}

void simrt_basicio_outreal_ex(
    void *object, double value, int64_t n, int64_t w, int64_t exp_digits
) {
    char item[128];
    unsigned char field[SIMRT_NUMERIC_FIELD_MAX];
    size_t item_len =
        simrt_format_real_ex(item, sizeof(item), value, n, (int)exp_digits);
    size_t field_len;
    simrt_pad_numeric_field(field, &field_len, item, item_len, w);
    basicio_out_formatted(object, (const char *)field, field_len);
}

void simrt_basicio_outreal(void *object, double value, int64_t n, int64_t w) {
    simrt_basicio_outreal_ex(object, value, n, w, 2);
}

void simrt_basicio_outfix(void *object, double value, int64_t n, int64_t w) {
    char item[128];
    unsigned char field[SIMRT_NUMERIC_FIELD_MAX];
    size_t item_len = simrt_format_fix(item, sizeof(item), value, n);
    size_t field_len;
    simrt_pad_numeric_field(field, &field_len, item, item_len, w);
    basicio_out_formatted(object, (const char *)field, field_len);
}

void simrt_basicio_outfrac(void *object, int64_t value, int64_t n, int64_t w) {
    char item[128];
    unsigned char field[SIMRT_NUMERIC_FIELD_MAX];
    size_t item_len = simrt_format_frac(item, sizeof(item), value, n);
    size_t field_len;
    simrt_pad_numeric_field(field, &field_len, item, item_len, w);
    basicio_out_formatted(object, (const char *)field, field_len);
}

void simrt_basicio_outint(void *object, int64_t value, int64_t w) {
    char digits[32];
    unsigned char field[SIMRT_NUMERIC_FIELD_MAX];
    int n = snprintf(digits, sizeof(digits), "%lld", (long long)value);
    size_t dig_len;
    size_t field_len;
    if (n < 0) {
        return;
    }
    dig_len = (size_t)n < sizeof(digits) ? (size_t)n : sizeof(digits) - 1;
    if (w == 0) {
        basicio_out_formatted(object, digits, dig_len);
        return;
    }
    /* pad_numeric_field asterisk-fills on overflow (Standard §10.5.8). */
    simrt_pad_numeric_field(field, &field_len, digits, dig_len, w);
    basicio_out_formatted(object, (const char *)field, field_len);
}

/* PrintFile `line` (§10.7) — current line number on the page. */
int64_t simrt_basicio_line(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "line");
    return file->line;
}

/* BASICIO `setaccess(mode)` (§10.1.1). Returns 1 if the mode text was
 * recognized, else 0. */
int32_t simrt_basicio_setaccess(void *object, SimrtTextFrame *mode_frame) {
    SimrtBasicioFile *file = basicio_require(object, "setaccess");
    size_t len = simrt_text_content_length(mode_frame);
    const unsigned char *ptr = simrt_text_content_ptr(mode_frame);
    char buf[64];
    size_t i;
    size_t n = len < sizeof(buf) - 1 ? len : sizeof(buf) - 1;
    if (ptr == NULL && n > 0) {
        return 0;
    }
    for (i = 0; i < n; i++) {
        unsigned char c = ptr[i];
        if (c >= 'A' && c <= 'Z') {
            c = (unsigned char)(c - 'A' + 'a');
        }
        buf[i] = (char)c;
    }
    buf[n] = '\0';
    /* Trim trailing blanks. */
    while (n > 0 && buf[n - 1] == ' ') {
        buf[--n] = '\0';
    }
    if (strcmp(buf, "append") == 0) {
        file->append = 1;
        return 1;
    }
    if (strcmp(buf, "noappend") == 0) {
        file->append = 0;
        return 1;
    }
    if (strcmp(buf, "create") == 0) {
        file->create_mode = 1;
        return 1;
    }
    if (strcmp(buf, "nocreate") == 0) {
        file->create_mode = 2;
        return 1;
    }
    if (strcmp(buf, "anycreate") == 0) {
        file->create_mode = 0;
        return 1;
    }
    if (strcmp(buf, "shared") == 0
        || strcmp(buf, "noshared") == 0
        || strcmp(buf, "readonly") == 0
        || strcmp(buf, "writeonly") == 0
        || strcmp(buf, "readwrite") == 0
        || strcmp(buf, "rewind") == 0
        || strcmp(buf, "norewind") == 0
        || strcmp(buf, "purge") == 0
        || strcmp(buf, "nopurge") == 0) {
        return 1; /* accepted, no-op for host open */
    }
    if (strncmp(buf, "bytesize:", 9) == 0) {
        return 1;
    }
    return 0;
}

/* BASICIO `eject(n)` (§10.7.1). */
void simrt_basicio_eject(void *object, int64_t n) {
    SimrtBasicioFile *file = basicio_require(object, "eject");
    if (!file->open) {
        simrt_error("eject: file is not open");
    }
    if (n <= 0) {
        simrt_error("eject: parameter out of range");
    }
    if (file->lines_per_page > 0 && n > file->lines_per_page) {
        n = 1;
    }
    if (n <= file->line) {
        if (file->fp != NULL) {
            fputc('\n', file->fp);
            fflush(file->fp);
        } else if (file->is_terminal) {
            /* Form-feed marker for SYSOUT is a blank line in this host. */
            simrt_out_text((const unsigned char *)"\n", 1);
        }
        file->page += 1;
    }
    file->line = n;
}

/* BASICIO `linesperpage(n)` (§10.7) — set page length; return previous. */
int64_t simrt_basicio_linesperpage(void *object, int64_t n) {
    SimrtBasicioFile *file = basicio_require(object, "linesperpage");
    int64_t prev = file->lines_per_page > 0 ? file->lines_per_page : 60;
    if (file->default_lines_per_page <= 0) {
        file->default_lines_per_page = 60;
    }
    if (n > 0) {
        file->lines_per_page = n;
    } else if (n < 0) {
        file->lines_per_page = INT64_MAX;
    } else {
        file->lines_per_page = file->default_lines_per_page;
    }
    return prev;
}

/* BASICIO `inrecord` (§10.4.2) — no space-fill; returns 1 if truncated. */
int32_t simrt_basicio_inrecord(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "inrecord");
    char line[8192];
    size_t n;
    size_t capacity;
    size_t take;
    size_t i;
    int truncated;
    if (!file->open || file->endfile || file->fp == NULL) {
        simrt_error("inrecord: file closed or at endfile");
    }
    if (fgets(line, (int)sizeof(line), file->fp) == NULL) {
        file->endfile = 1;
        file->image_pos = 1;
        if (file->image_len > 0) {
            file->image[0] = (unsigned char)'!'; /* EM surrogate MVP */
        }
        return 0;
    }
    n = strlen(line);
    while (n > 0 && (line[n - 1] == '\n' || line[n - 1] == '\r')) {
        n--;
    }
    capacity = file->image_len;
    truncated = n > capacity;
    take = n < capacity ? n : capacity;
    for (i = 0; i < take; i++) {
        file->image[i] = (unsigned char)line[i];
    }
    file->image_pos = (size_t)take + 1;
    return truncated ? 1 : 0;
}

/* BASICIO `filename` (§10.1) — constructor path as a mutable text value. */
SimrtTextFrame *simrt_basicio_filename(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "filename");
    size_t len = file->path != NULL ? strlen(file->path) : 0;
    return simrt_text_from_literal(
        (const unsigned char *)(file->path != NULL ? file->path : ""), len
    );
}

/* BASICIO `image` (§10.3) — current image content up to `image_len`.
 * Terminal SysOut/SysIn share the process-wide free-image buffers (`line` /
 * `sysin_line`) that OutText/OutInt/InImage already use; non-terminal files
 * keep a private `file->image` buffer established by `open(fileimage)`. */
SimrtTextFrame *simrt_basicio_image(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "image");
    simrt_image_buffers_ensure_init();
    if (file->is_terminal) {
        if (file->mode == 0) {
            return simrt_text_from_literal(sysin_line, sysin_len);
        }
        return simrt_text_from_literal(line, line_len);
    }
    return simrt_text_from_literal(file->image, file->image_len);
}

/* BASICIO `image :- text` / `image := text` — replace the current image
 * content. Terminals update the free-image line buffer (length = source
 * content length, capped at the static buffer); other files blank-pad /
 * truncate into the fixed `open` image without resizing `image_len`. */
void simrt_basicio_set_image(void *object, SimrtTextFrame *text) {
    SimrtBasicioFile *file = basicio_require(object, "image");
    size_t len = simrt_text_content_length(text);
    const unsigned char *ptr = simrt_text_content_ptr(text);
    if (file->is_terminal) {
        unsigned char *buf = file->mode == 0 ? sysin_line : line;
        size_t capacity = file->mode == 0 ? sizeof(sysin_line) : sizeof(line);
        size_t *buf_len = file->mode == 0 ? &sysin_len : &line_len;
        size_t *buf_pos = file->mode == 0 ? &sysin_pos : &line_pos;
        size_t copy_len = len < capacity ? len : capacity;
        if (copy_len > 0 && ptr != NULL) {
            memcpy(buf, ptr, copy_len);
        }
        *buf_len = copy_len;
        *buf_pos = 1;
        if (file->mode == 0) {
            sysin_endfile = 0;
            sysin_image_len = copy_len;
        }
        return;
    }
    size_t copy_len = len < file->image_len ? len : file->image_len;
    memset(file->image, ' ', file->image_len);
    if (copy_len > 0 && ptr != NULL) {
        memcpy(file->image, ptr, copy_len);
    }
    file->image_pos = 1;
}

/* BASICIO `setpos(i)` (§10.3) — clamp like text.setpos against image_len. */
void simrt_basicio_setpos(void *object, int64_t i) {
    SimrtBasicioFile *file = basicio_require(object, "setpos");
    if (file->is_terminal) {
        /* SysOut/SysIn free image: reuse the process-wide line_pos / sysin_pos. */
        size_t *pos = (file->mode == 0) ? &sysin_pos : &line_pos;
        size_t len = (file->mode == 0) ? sysin_len : line_len;
        if (i <= 0) {
            *pos = 1;
        } else if ((size_t)i > len + 1) {
            *pos = len + 1;
        } else {
            *pos = (size_t)i;
        }
        return;
    }
    if (i <= 0) {
        file->image_pos = 1;
    } else if ((size_t)i > file->image_len + 1) {
        file->image_pos = file->image_len + 1;
    } else {
        file->image_pos = (size_t)i;
    }
}

int64_t simrt_basicio_pos(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "pos");
    if (file->is_terminal) {
        simrt_image_buffers_ensure_init();
        return (int64_t)((file->mode == 0) ? sysin_pos : line_pos);
    }
    return (int64_t)(file->image_pos > 0 ? file->image_pos : 1);
}

int64_t simrt_basicio_length(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "length");
    if (file->is_terminal) {
        simrt_image_buffers_ensure_init();
        return (int64_t)((file->mode == 0) ? sysin_len : line_len);
    }
    return (int64_t)file->image_len;
}

int32_t simrt_basicio_inchar(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "inchar");
    size_t idx;
    if (file->is_terminal) {
        return simrt_in_char();
    }
    if (!file->open) {
        simrt_error("InFile.inchar: file is not open");
    }
    if (file->image_pos > file->image_len) {
        simrt_basicio_inimage(object);
    }
    if (file->endfile && file->image_pos > file->image_len) {
        simrt_error("InChar: end of file");
    }
    idx = file->image_pos > 0 ? file->image_pos - 1 : 0;
    if (idx >= file->image_len) {
        simrt_error("InChar: no more characters in image");
    }
    file->image_pos++;
    return (int32_t)file->image[idx];
}

/* Peek/skip spaces for lastitem / item-oriented input (§10.4). Uses inchar so
 * DirectFile sequential reads auto-advance via inimage when past the image. */
static int32_t basicio_skip_spaces(void *object, SimrtBasicioFile *file) {
    int32_t ch = (int32_t)' ';
    while (!file->endfile && (ch == (int32_t)' ' || ch == (int32_t)'\t')) {
        if (file->is_terminal) {
            if (sysin_endfile) {
                break;
            }
            if (sysin_pos > sysin_len) {
                simrt_in_image();
            }
            if (sysin_endfile && sysin_pos > sysin_len) {
                break;
            }
        } else if (file->image_pos > file->image_len) {
            simrt_basicio_inimage(object);
            if (file->endfile && file->image_pos > file->image_len) {
                break;
            }
        }
        ch = simrt_basicio_inchar(object);
    }
    return ch;
}

int32_t simrt_basicio_lastitem(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "lastitem");
    int32_t ch;
    if (file->is_terminal) {
        simrt_image_buffers_ensure_init();
        ch = basicio_skip_spaces(object, file);
        if (!sysin_endfile && ch != (int32_t)' ' && ch != (int32_t)'\t' && sysin_pos > 1) {
            sysin_pos -= 1;
        }
        return sysin_endfile ? 1 : 0;
    }
    if (!file->open) {
        simrt_error("lastitem: file is not open");
    }
    ch = basicio_skip_spaces(object, file);
    if (!file->endfile && ch != (int32_t)' ' && ch != (int32_t)'\t' && file->image_pos > 1) {
        file->image_pos -= 1;
    }
    return file->endfile ? 1 : 0;
}

static void basicio_remaining_image(
    SimrtBasicioFile *file, const unsigned char **ptr_out, size_t *len_out
) {
    size_t pos = file->image_pos > 0 ? file->image_pos : 1;
    if (pos > file->image_len) {
        *ptr_out = file->image;
        *len_out = 0;
        return;
    }
    *ptr_out = file->image + (pos - 1);
    *len_out = file->image_len - (pos - 1);
}

int64_t simrt_basicio_inint(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "inint");
    const unsigned char *ptr;
    size_t len;
    int64_t value = 0;
    size_t consumed = 0;
    if (simrt_basicio_lastitem(object)) {
        simrt_error("inint: end of file");
    }
    if (file->is_terminal) {
        SimrtTextFrame *frame = simrt_basicio_image(object);
        /* Align text frame pos with sysin_pos for getint. */
        if (frame != NULL) {
            frame->pos = (int64_t)sysin_pos;
        }
        value = simrt_text_getint(frame);
        sysin_pos = (size_t)frame->pos;
        return value;
    }
    basicio_remaining_image(file, &ptr, &len);
    if (!simrt_text_parse_integer_item(ptr, len, &value, &consumed)) {
        simrt_error("inint: no numeric item");
    }
    file->image_pos += consumed;
    return value;
}

double simrt_basicio_inreal(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "inreal");
    const unsigned char *ptr;
    size_t len;
    double value = 0.0;
    size_t consumed = 0;
    if (simrt_basicio_lastitem(object)) {
        simrt_error("inreal: end of file");
    }
    if (file->is_terminal) {
        SimrtTextFrame *frame = simrt_basicio_image(object);
        if (frame != NULL) {
            frame->pos = (int64_t)sysin_pos;
        }
        value = simrt_text_getreal(frame);
        sysin_pos = (size_t)frame->pos;
        return value;
    }
    basicio_remaining_image(file, &ptr, &len);
    if (!simrt_text_parse_real_item(ptr, len, &value, &consumed)) {
        simrt_error("inreal: no numeric item");
    }
    file->image_pos += consumed;
    return value;
}

int64_t simrt_basicio_infrac(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "infrac");
    SimrtTextFrame *frame;
    int64_t value;
    if (simrt_basicio_lastitem(object)) {
        simrt_error("infrac: end of file");
    }
    frame = simrt_basicio_image(object);
    if (file->is_terminal) {
        if (frame != NULL) {
            frame->pos = (int64_t)sysin_pos;
        }
        value = simrt_text_getfrac(frame);
        sysin_pos = (size_t)frame->pos;
        return value;
    }
    if (frame != NULL) {
        frame->pos = (int64_t)file->image_pos;
    }
    value = simrt_text_getfrac(frame);
    file->image_pos = (size_t)frame->pos;
    return value;
}

SimrtTextFrame *simrt_basicio_intext(void *object, int64_t w) {
    SimrtTextFrame *t;
    int64_t i;
    (void)basicio_require(object, "intext");
    if (w <= 0) {
        return simrt_text_notext();
    }
    t = simrt_text_blanks(w);
    for (i = 0; i < w; i++) {
        int32_t ch = simrt_basicio_inchar(object);
        simrt_text_putchar(t, ch);
    }
    return t;
}

int32_t simrt_basicio_endfile(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "endfile");
    if (file->is_terminal) {
        return simrt_endfile();
    }
    return file->endfile ? 1 : 0;
}

int32_t simrt_basicio_inbyte(void *object) {
    SimrtBasicioFile *file = basicio_require(object, "inbyte");
    int ch;
    if (file->mode != 2) {
        simrt_error("inbyte: not an inbytefile");
    }
    if (!file->open || file->fp == NULL) {
        simrt_error("inbyte: file is not open");
    }
    if (file->endfile) {
        simrt_error("inbyte: end of file");
    }
    ch = fgetc(file->fp);
    if (ch == EOF) {
        file->endfile = 1;
        return 0;
    }
    return (int32_t)(ch & 0xFF);
}

void simrt_basicio_outbyte(void *object, int64_t x) {
    SimrtBasicioFile *file = basicio_require(object, "outbyte");
    if (file->mode != 3) {
        simrt_error("outbyte: not an outbytefile");
    }
    if (!file->open || file->fp == NULL) {
        simrt_error("outbyte: file is not open");
    }
    if (x < 0 || x > 255) {
        simrt_error("outbyte: illegal byte value");
    }
    if (fputc((int)x, file->fp) == EOF) {
        simrt_error("outbyte: write failed");
    }
}

void simrt_terminate_program(void) {
    size_t i;
    for (i = 0; i < g_basicio_file_count; i++) {
        SimrtBasicioFile *file = &g_basicio_files[i];
        if (file->open && file->fp != NULL && !file->is_terminal) {
            fclose(file->fp);
            file->fp = NULL;
            file->open = 0;
        }
    }
    exit(0);
}

int simrt_file_exists(SimrtTextFrame *path_frame) {
    char *path = simrt_text_to_cstr(path_frame);
#ifdef _WIN32
    int ok = _access(path, 0) == 0;
#else
    int ok = access(path, F_OK) == 0;
#endif
    free(path);
    return ok;
}

SimrtTextFrame *simrt_file_read(SimrtTextFrame *path_frame) {
    char *path = simrt_text_to_cstr(path_frame);
    FILE *file = fopen(path, "rb");
    if (file == NULL) {
        fprintf(stderr, "sim: path not found: %s\n", path);
        free(path);
        abort();
    }
    free(path);
    if (fseek(file, 0, SEEK_END) != 0) {
        fprintf(stderr, "sim: failed to read file\n");
        fclose(file);
        abort();
    }
    long size_long = ftell(file);
    if (size_long < 0) {
        fprintf(stderr, "sim: failed to read file\n");
        fclose(file);
        abort();
    }
    size_t size = (size_t)size_long;
    rewind(file);
    unsigned char *buf = NULL;
    if (size > 0) {
        buf = (unsigned char *)simrt_host_malloc(size);
        if (buf == NULL) {
            fclose(file);
            simrt_text_oom();
        }
        size_t nread = fread(buf, 1, size, file);
        if (nread != size) {
            fprintf(stderr, "sim: failed to read file\n");
            free(buf);
            fclose(file);
            abort();
        }
    }
    fclose(file);
    SimrtTextFrame *frame = simrt_text_from_literal(buf == NULL ? (const unsigned char *)"" : buf, size);
    free(buf);
    return frame;
}

void simrt_file_write(SimrtTextFrame *path_frame, SimrtTextFrame *contents_frame) {
    char *path = simrt_text_to_cstr(path_frame);
    FILE *file = fopen(path, "wb");
    if (file == NULL) {
        fprintf(stderr, "sim: failed to write file: %s\n", path);
        free(path);
        abort();
    }
    free(path);
    size_t len = simrt_text_content_length(contents_frame);
    const unsigned char *ptr = simrt_text_content_ptr(contents_frame);
    if (len > 0 && ptr != NULL) {
        if (fwrite(ptr, 1, len, file) != len) {
            fprintf(stderr, "sim: failed to write file\n");
            fclose(file);
            abort();
        }
    }
    if (fclose(file) != 0) {
        fprintf(stderr, "sim: failed to write file\n");
        abort();
    }
}

void simrt_basicio_gc_visit_roots(simrt_gc_mark_fn mark) {
    size_t i;
    if (mark == NULL) {
        return;
    }
    mark(g_sysin_obj);
    mark(g_sysout_obj);
    for (i = 0; i < g_basicio_file_count; i++) {
        /* Retired slots keep their object so `isopen` / `filename` / a reopen
         * still resolve for it, which makes them roots too. */
        if (g_basicio_files[i].object != NULL) {
            mark(g_basicio_files[i].object);
        }
    }
}
