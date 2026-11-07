// Keep Cargo's process group, stdio and working directory while assigning
// screen-capture requests to the signed app instead of its terminal.
#include <crt_externs.h>
#include <dlfcn.h>
#include <spawn.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

typedef pid_t (*ResponsiblePid)(pid_t);

bool xrose_owns_app_identity(void) {
    ResponsiblePid owner = dlsym(RTLD_DEFAULT, "responsibility_get_pid_responsible_for_pid");
    return getppid() == 1 || (owner && owner(getpid()) == getpid());
}

// These optional libSystem entry points are not in the public SDK. If they
// are unavailable or spawning fails, the Rust caller uses Launch Services.
void xrose_exec_bundle(const char *executable) {
    int (*disclaim)(posix_spawnattr_t *, int) =
        dlsym(RTLD_DEFAULT, "responsibility_spawnattrs_setdisclaim");
    if (!disclaim || !dlsym(RTLD_DEFAULT, "responsibility_get_pid_responsible_for_pid"))
        return;
    posix_spawnattr_t attr;
    if (posix_spawnattr_init(&attr) != 0)
        return;
    if (posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETEXEC) == 0 && disclaim(&attr, 1) == 0) {
        size_t bytes = ((size_t)*_NSGetArgc() + 1) * sizeof(char *);
        char **args = malloc(bytes);
        if (args) {
            memcpy(args, *_NSGetArgv(), bytes);
            args[0] = (char *)executable;
            posix_spawn(NULL, executable, NULL, &attr, args, *_NSGetEnviron());
            free(args);
        }
    }
    posix_spawnattr_destroy(&attr);
}
