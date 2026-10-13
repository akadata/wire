CC = gcc
CFLAGS_O3 = -O3 -Wall -Wextra
CFLAGS_NATIVE = -O3 -march=native -Wall -Wextra

all: task_bench_o3 task_bench_native

task_bench_o3: task_bench.c
	$(CC) $(CFLAGS_O3) -o $@ $<

task_bench_native: task_bench.c
	$(CC) $(CFLAGS_NATIVE) -o $@ $<

clean:
	rm -f task_bench_o3 task_bench_native
