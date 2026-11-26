extern void _Exit(int);
static volatile char early_data[] = "required-before-runtime";
static volatile char late_data[] = "encrypt-after-runtime";
static volatile int observed;
__attribute__((noinline)) static void early_helper(void) {
    if (early_data[0] != 'r' || early_data[22] != 'e') _Exit(73);
    observed = 1;
}
static void (* volatile callback)(void) = early_helper;
__attribute__((constructor(0),section(".text.lyra.preinit"))) static void early_root(void) {
    callback();
}
int main(void) {
    return observed != 1 || late_data[0] != 'e' || late_data[20] != 'e' ? 74 : 0;
}
