/* CRT process entry for AOT binaries. Not compiled into C fixtures, which
 * provide their own `main`. Cranelift exports `sim_main`. */

int sim_main(void);

int main(void) {
    return sim_main();
}
