/**
 * darwin/process.c — Process name lookup via libproc
 */
#include <libproc.h>
#include <string.h>

/** Fill `name` (at least PROC_PIDPATHINFO_MAXSIZE bytes) with the process name. */
int ml_darwin_proc_name(pid_t pid, char *name, uint32_t size) {
    return proc_name((int)pid, name, size);
}
