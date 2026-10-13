/*
 * Pure-C port of the Wire LocalExecutor microbenchmark (AKADATA).
 *
 * Mirrors wire-runtime's design on purpose so the comparison measures
 * language/framework overhead, not a different algorithm:
 *   - MAX_TASKS = 4096 fixed slots, free-list stack, circular run queue
 *   - spawn(): pop free slot, heap-allocate one task object (like Box),
 *     enqueue; returns -1 when full (like spawn() -> None)
 *   - drain: dequeue, indirect call through function pointer (like dyn
 *     Runnable dispatch), free object, recycle slot, count completions
 *   - workload: 1,000,000 no-op tasks, single thread, BATCH = 1024,
 *     drain each batch fully, spawn+dispatch+completion inside elapsed
 *   - same fail-closed contract: valid only if
 *     attempted == accepted == completed == 1,000,000, else exit non-zero
 *   - thread pinned to core 0, like LocalExecutor::new(0)
 *
 * Known asymmetries (documented, not hidden): this port has no futures,
 * no wakers, no Pending path, no thread-local executor guard and no
 * io_uring submit per drain. It is a lean dispatch loop — which is exactly
 * the point: it shows what the abstraction layers cost on this workload.
 * Same glibc malloc as the Rust build (Rust uses the system allocator).
 *
 * Build:
 *   gcc -O3 -o task_bench_o3 task_bench.c
 *   gcc -O3 -march=native -o task_bench_native task_bench.c
 */
#define _GNU_SOURCE

#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

#define MAX_TASKS 4096
#define TOTAL 1000000
#define BATCH 1024

typedef void (*task_fn)(void *arg);

typedef struct {
    task_fn fn;
    void *arg;
} task_t;

static task_t *slots[MAX_TASKS];
static int free_stack[MAX_TASKS];
static int free_top; /* number of free slots */
static int runq[MAX_TASKS];
static int rq_head;
static int rq_len;

static void task_noop(void *arg) {
    (void)arg;
}

static int spawn_task(void) {
    if (free_top == 0) {
        return -1; /* queue full */
    }
    int id = free_stack[--free_top];
    task_t *t = (task_t *)malloc(sizeof(task_t));
    if (t == NULL) {
        free_stack[free_top++] = id; /* restore slot */
        return -1;
    }
    t->fn = task_noop;
    t->arg = NULL;
    slots[id] = t;
    runq[(rq_head + rq_len) & (MAX_TASKS - 1)] = id;
    rq_len++;
    return id;
}

/* Drain the whole queue; returns completions, or -1 if count != expected. */
static long drain_batch(long expected) {
    long n = 0;
    while (rq_len > 0) {
        int id = runq[rq_head];
        rq_head = (rq_head + 1) & (MAX_TASKS - 1);
        rq_len--;
        task_t *t = slots[id];
        slots[id] = NULL;
        t->fn(t->arg);
        free(t);
        free_stack[free_top++] = id;
        n++;
    }
    return (n == expected) ? n : -1;
}

static double elapsed_s(const struct timespec *a, const struct timespec *b) {
    return (double)(b->tv_sec - a->tv_sec) +
           (double)(b->tv_nsec - a->tv_nsec) / 1000000000.0;
}

int main(void) {
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(0, &set);
    (void)sched_setaffinity(0, sizeof(set), &set); /* best effort, like Rust */

    for (int i = 0; i < MAX_TASKS; i++) {
        free_stack[i] = MAX_TASKS - 1 - i;
        slots[i] = NULL;
    }
    free_top = MAX_TASKS;
    rq_head = 0;
    rq_len = 0;

    const long attempted = TOTAL;
    long accepted = 0;
    long completed = 0;
    int valid = 1;
    long in_batch = 0;

    struct timespec t0, t1;
    (void)clock_gettime(CLOCK_MONOTONIC, &t0);
    for (long i = 0; i < TOTAL; i++) {
        if (spawn_task() < 0) {
            valid = 0; /* full despite BATCH << MAX_TASKS: fail closed */
            break;
        }
        accepted++;
        in_batch++;
        if (in_batch == BATCH) {
            long n = drain_batch(in_batch);
            if (n < 0) {
                valid = 0;
                break;
            }
            completed += n;
            in_batch = 0;
        }
    }
    if (valid && in_batch > 0) {
        long n = drain_batch(in_batch);
        if (n < 0) {
            valid = 0;
        } else {
            completed += n;
        }
    }
    (void)clock_gettime(CLOCK_MONOTONIC, &t1);

    double el = elapsed_s(&t0, &t1);
    if (accepted != attempted || completed != attempted) {
        valid = 0;
    }
    double mops = 0.0, ns = 0.0;
    if (valid && completed > 0) {
        mops = ((double)completed / el) / 1000000.0;
        ns = (el * 1000000000.0) / (double)completed;
    }
    printf("EXECUTOR attempted=%ld accepted=%ld completed=%ld "
           "elapsed_s=%.6f mops=%.4f ns_per_task=%.2f valid=%s\n",
           attempted, accepted, completed, el, mops, ns,
           valid ? "true" : "false");
    if (!valid) {
        (void)fprintf(stderr,
                      "[FAIL] executor accounting mismatch: "
                      "attempted=%ld accepted=%ld completed=%ld "
                      "(require 1000000/1000000/1000000)\n",
                      attempted, accepted, completed);
        return 1;
    }
    return 0;
}
