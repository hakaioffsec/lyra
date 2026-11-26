typedef unsigned long pthread_t;
extern int pthread_create(pthread_t *, const void *, void *(*)(void *), void *);
extern int pthread_join(pthread_t, void **);
static __thread volatile unsigned char state[4] = {17, 34, 51, 68};
static void *worker(void *unused) {
    (void)unused;
    return (void *)(unsigned long)(state[0] != 17 || state[1] != 34 || state[2] != 51 || state[3] != 68);
}
int main(void) {
    pthread_t thread;
    void *result = 0;
    if (state[0] != 17 || state[3] != 68) return 70;
    if (pthread_create(&thread, 0, worker, 0) || pthread_join(thread, &result)) return 71;
    return result ? 72 : 0;
}
