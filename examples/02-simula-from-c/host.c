#include <stdint.h>
#include <stdio.h>

int64_t sim_add(int64_t a, int64_t b);

int main(void) {
    printf("%lld\n", (long long)sim_add(40, 2));
    return 0;
}
