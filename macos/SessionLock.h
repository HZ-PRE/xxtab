#ifndef XXTAB_SESSION_LOCK_H
#define XXTAB_SESSION_LOCK_H

#include <sys/file.h>

// Give Swift an unambiguous function name: Darwin also exposes struct flock.
// Keep BSD flock semantics identical to the Rust worker's libc::flock calls.
static inline int xxtab_flock(int fd, int operation) {
    return flock(fd, operation);
}

#endif
