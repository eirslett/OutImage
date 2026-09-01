#include "embed.h"

#include <stdint.h>
#include <stdio.h>

static int64_t host_add(int64_t a, int64_t b) { return a + b; }

int main(void) {
    SimrtHostDef host[] = {{"add", (void *)host_add}};
    SimrtInstance *s = simrt_instantiate(host, 1);
    SimrtVal result;
    if (simrt_call(s, "sim_combo", NULL, 0, &result) != 0) {
        fprintf(stderr, "simrt_call failed\n");
        return 1;
    }
    printf("%lld\n", (long long)result.u.i64);
    printf("%.1f\n", simrt_sim_now(s));
    printf("%d\n", simrt_sim_step(s));
    simrt_release(s);
    return 0;
}
