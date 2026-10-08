/* measure: runs one command from a very small parent and records what it cost.

   usage: measure -r RESULT [-o STDOUT] [-t SECONDS] -- COMMAND ARGS...

   RESULT gets one line: "rc wall user sys maxrss_kb timed_out" (rc is the exit code, or minus the signal
   number). -o sends the command's standard output to a file (for tools that write their result there).
   -t kills the command after that many seconds and marks the line timed out.

   Why a separate program: Linux starts a new process with the peak memory of the process that forked it
   (the resident pages it inherits count), so a 20 MB Python parent would put a 20 MB floor under every
   memory reading. This parent is under 1 MB. benchmark.py compiles it with cc when it is run. */
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static pid_t child = 0;
static volatile sig_atomic_t timed_out = 0;

static void on_alarm(int sig) {
    (void)sig;
    timed_out = 1;
    if (child > 0)
        kill(child, SIGKILL);
}

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (double)t.tv_sec + (double)t.tv_nsec / 1e9;
}

int main(int argc, char **argv) {
    const char *result = NULL, *out = NULL;
    unsigned timeout = 0;
    int i = 1;
    for (; i < argc; i++) {
        if (strcmp(argv[i], "--") == 0) { i++; break; }
        if (strcmp(argv[i], "-r") == 0 && i + 1 < argc) result = argv[++i];
        else if (strcmp(argv[i], "-o") == 0 && i + 1 < argc) out = argv[++i];
        else if (strcmp(argv[i], "-t") == 0 && i + 1 < argc) timeout = (unsigned)strtoul(argv[++i], NULL, 10);
        else { fprintf(stderr, "measure: bad option %s\n", argv[i]); return 2; }
    }
    if (i >= argc || result == NULL) {
        fprintf(stderr, "usage: measure -r RESULT [-o STDOUT] [-t SECONDS] -- COMMAND ARGS...\n");
        return 2;
    }
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_alarm;
    sigaction(SIGALRM, &sa, NULL);
    double t0 = now();
    child = fork();
    if (child < 0) { perror("measure: fork"); return 2; }
    if (child == 0) {
        if (out != NULL) {
            int fd = open(out, O_WRONLY | O_CREAT | O_TRUNC, 0644);
            if (fd < 0) { perror("measure: open"); _exit(126); }
            dup2(fd, 1);
            close(fd);
        }
        execvp(argv[i], argv + i);
        perror("measure: exec");
        _exit(127);
    }
    if (timeout > 0)
        alarm(timeout);
    int status = 0;
    struct rusage ru;
    while (wait4(child, &status, 0, &ru) < 0) {
        if (errno == EINTR)
            continue; /* the alarm interrupted the wait; wait again for the killed child */
        perror("measure: wait4");
        return 2;
    }
    double wall = now() - t0;
    alarm(0);
    int rc = WIFEXITED(status) ? WEXITSTATUS(status) : (WIFSIGNALED(status) ? -WTERMSIG(status) : -1);
    FILE *f = fopen(result, "w");
    if (f == NULL) { perror("measure: result"); return 2; }
    fprintf(f, "%d %.6f %.6f %.6f %ld %d\n", rc, wall, (double)ru.ru_utime.tv_sec + ru.ru_utime.tv_usec / 1e6,
            (double)ru.ru_stime.tv_sec + ru.ru_stime.tv_usec / 1e6, ru.ru_maxrss, (int)timed_out);
    fclose(f);
    return 0;
}
