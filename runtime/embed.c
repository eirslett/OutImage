#include <string.h>

#include "embed.h"
#include "internal.h"

#define SIMRT_HOST_MAX 64
#define SIMRT_EXPORT_MAX 64
#define SIMRT_NAME_MAX 64

typedef struct {
    char name[SIMRT_NAME_MAX];
    void *fn;
} SimrtNamedFn;

typedef struct {
    char name[SIMRT_NAME_MAX];
    void *fn;
    int32_t sig;
} SimrtExportEntry;

struct SimrtInstance {
    int alive;
};

static struct SimrtInstance g_instance;
static SimrtNamedFn g_host[SIMRT_HOST_MAX];
static int g_host_n;
static SimrtExportEntry g_exports[SIMRT_EXPORT_MAX];
static int g_export_n;

/* Provided by Cranelift for every native artifact. */
void simrt_module_init(void);

static int simrt_name_copy(char *dest, const char *name) {
    size_t n;
    if (name == NULL || name[0] == '\0') {
        return 0;
    }
    n = strlen(name);
    if (n >= SIMRT_NAME_MAX) {
        return 0;
    }
    memcpy(dest, name, n + 1);
    return 1;
}

void simrt_host_define(const char *name, void *fn) {
    int i;
    char tmp[SIMRT_NAME_MAX];
    if (fn == NULL || !simrt_name_copy(tmp, name)) {
        simrt_error("invalid Host registration");
    }
    for (i = 0; i < g_host_n; i++) {
        if (strcmp(g_host[i].name, name) == 0) {
            g_host[i].fn = fn;
            return;
        }
    }
    if (g_host_n >= SIMRT_HOST_MAX) {
        simrt_error("too many Host procedures");
    }
    memcpy(g_host[g_host_n].name, tmp, SIMRT_NAME_MAX);
    g_host[g_host_n].fn = fn;
    g_host_n += 1;
}

void *simrt_host_resolve(const char *name) {
    int i;
    if (name == NULL) {
        simrt_error("unresolved Host procedure");
    }
    for (i = 0; i < g_host_n; i++) {
        if (strcmp(g_host[i].name, name) == 0) {
            return g_host[i].fn;
        }
    }
    simrt_error("unresolved Host procedure");
    return NULL;
}

void simrt_register_export(const char *name, void *fn, int32_t sig) {
    if (fn == NULL || name == NULL || g_export_n >= SIMRT_EXPORT_MAX) {
        return;
    }
    if (!simrt_name_copy(g_exports[g_export_n].name, name)) {
        return;
    }
    g_exports[g_export_n].fn = fn;
    g_exports[g_export_n].sig = sig;
    g_export_n += 1;
}

SimrtInstance *simrt_instantiate(const SimrtHostDef *host, int nhost) {
    int i;
    if (g_instance.alive) {
        simrt_error("simrt_instantiate: already instantiated");
    }
    g_host_n = 0;
    g_export_n = 0;
    if (host != NULL) {
        for (i = 0; i < nhost; i++) {
            simrt_host_define(host[i].name, host[i].fn);
        }
    }
    (void)simrt_sysin();
    (void)simrt_sysout();
    simrt_module_init();
    g_instance.alive = 1;
    return &g_instance;
}

void simrt_release(SimrtInstance *instance) {
    if (instance == NULL) {
        return;
    }
    instance->alive = 0;
    g_host_n = 0;
    g_export_n = 0;
}

static int simrt_sig_nargs(int32_t sig) {
    return (sig >> 20) & 0xf;
}

static int simrt_sig_slot(int32_t sig, int slot) {
    return (sig >> (4 * slot)) & 0xf;
}

int simrt_call(
    SimrtInstance *instance,
    const char *export_name,
    const SimrtVal *args,
    int nargs,
    SimrtVal *result
) {
    int i;
    int32_t sig;
    void *fn;
    if (instance == NULL || !instance->alive || export_name == NULL) {
        return -1;
    }
    for (i = 0; i < g_export_n; i++) {
        if (strcmp(g_exports[i].name, export_name) != 0) {
            continue;
        }
        sig = g_exports[i].sig;
        fn = g_exports[i].fn;
        if (simrt_sig_nargs(sig) != nargs) {
            return -1;
        }
        if (result != NULL) {
            result->kind = (SimrtValKind)simrt_sig_slot(sig, 0);
        }
        /* Packed: nibble 0 = result, 1.. = args. 1 = i64. */
        if (nargs == 0 && simrt_sig_slot(sig, 0) == SIMRT_VAL_I64) {
            int64_t value = ((int64_t (*)(void))fn)();
            if (result != NULL) {
                result->u.i64 = value;
            }
            return 0;
        }
        if (nargs == 2 && simrt_sig_slot(sig, 0) == SIMRT_VAL_I64
            && simrt_sig_slot(sig, 1) == SIMRT_VAL_I64
            && simrt_sig_slot(sig, 2) == SIMRT_VAL_I64) {
            int64_t a;
            int64_t b;
            int64_t value;
            if (args == NULL) {
                return -1;
            }
            a = args[0].u.i64;
            b = args[1].u.i64;
            value = ((int64_t (*)(int64_t, int64_t))fn)(a, b);
            if (result != NULL) {
                result->u.i64 = value;
            }
            return 0;
        }
        if (nargs == 1 && simrt_sig_slot(sig, 0) == SIMRT_VAL_REF
            && simrt_sig_slot(sig, 1) == SIMRT_VAL_REF) {
            SimrtRef value;
            if (args == NULL) {
                return -1;
            }
            value = ((SimrtRef (*)(SimrtRef))fn)(args[0].u.ref);
            if (result != NULL) {
                result->u.ref = value;
            }
            return 0;
        }
        if (nargs == 0 && simrt_sig_slot(sig, 0) == SIMRT_VAL_NONE) {
            ((void (*)(void))fn)();
            return 0;
        }
        (void)args;
        return -1;
    }
    return -1;
}

double simrt_sim_now(SimrtInstance *instance) {
    (void)instance;
    if (!simrt_sim_is_active()) {
        return 0.0;
    }
    return simrt_sim_time();
}

int simrt_sim_step(SimrtInstance *instance) {
    (void)instance;
    if (!simrt_sim_is_active()) {
        return 0;
    }
    return simrt_sim_has_current() ? 1 : 0;
}

int simrt_sim_run_until(SimrtInstance *instance, double time) {
    (void)time;
    return simrt_sim_step(instance);
}
