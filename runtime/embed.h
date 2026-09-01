/* Embedding API.
 *
 * A compiled `--crate-type lib` artifact links this into the shared library.
 * The host calls `simrt_instantiate`, then either the `sim_*` wrappers or
 * `simrt_call`. One Simula world per process in v1.
 */

#ifndef SIMRT_EMBED_H
#define SIMRT_EMBED_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    SIMRT_VAL_NONE = 0,
    SIMRT_VAL_I64 = 1,
    SIMRT_VAL_F64 = 2,
    SIMRT_VAL_BOOL = 3,
    SIMRT_VAL_CHAR = 4,
    SIMRT_VAL_REF = 5
} SimrtValKind;

typedef void *SimrtRef;

typedef struct {
    SimrtValKind kind;
    union {
        int64_t i64;
        double f64;
        int32_t boolean;
        uint32_t character;
        SimrtRef ref;
    } u;
} SimrtVal;

typedef struct {
    const char *name;
    void *fn;
} SimrtHostDef;

typedef struct SimrtInstance SimrtInstance;

/* `host` / `nhost` may be NULL / 0. Names match Host identifications. */
SimrtInstance *simrt_instantiate(const SimrtHostDef *host, int nhost);
void simrt_release(SimrtInstance *instance);
void simrt_host_define(const char *name, void *fn);

/* Lookup a registered export (`sim_add`, or an `export:` name). Returns 0
 * on success, -1 if the name/signature is unknown or the arity is wrong. */
int simrt_call(
    SimrtInstance *instance,
    const char *export_name,
    const SimrtVal *args,
    int nargs,
    SimrtVal *result
);

double simrt_sim_now(SimrtInstance *instance);
/* Native: 1 while a Simulation is active with a current process. Wasm hosts
 * should call the module's `step` export (asyncify trampoline) instead. */
int simrt_sim_step(SimrtInstance *instance);
int simrt_sim_run_until(SimrtInstance *instance, double time);

/* Pin a Simula object so the collector will not reclaim it while the host
 * holds the id. `simrt_ref_pin(NULL)` returns 0 (`none`). */
int64_t simrt_ref_pin(SimrtRef ref);
void simrt_ref_unpin(int64_t id);
SimrtRef simrt_ref_get(int64_t id);

#ifdef __cplusplus
}
#endif

#endif /* SIMRT_EMBED_H */
