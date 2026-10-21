/* App-managed policy: no includes, wildcards, or grants to other users.
 * Keep the ordinary QEMU helper's ACL language unchanged. */
#ifndef EINK_QEMU_BRIDGE_HELPER_ACL_H
#define EINK_QEMU_BRIDGE_HELPER_ACL_H

#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

static inline bool eink_bridge_acl_allowed(FILE *file, uid_t uid,
                                            const char *bridge)
{
    char data[65537], *line, *next;
    size_t length = fread(data, 1, sizeof(data), file);
    bool allowed = false;

    if (ferror(file) || length >= sizeof(data) || uid == 0 ||
        memchr(data, '\0', length)) {
        return false;
    }
    data[length] = '\0';
    for (line = data; *line; line = next + 1) {
        char command[16], user[32], name[16], extra;
        char *end;
        unsigned long long parsed;

        next = strchr(line, '\n');
        if (!next || next - line > 127) {
            return false;
        }
        *next = '\0';
        while (*line == ' ' || *line == '\t') {
            line++;
        }
        if (!*line || *line == '#') {
            continue;
        }
        if (sscanf(line, "%15s %31s %15s %c", command, user, name, &extra) != 3 ||
            strcmp(command, "allow") != 0 || !isdigit((unsigned char)user[0])) {
            return false;
        }
        errno = 0;
        parsed = strtoull(user, &end, 10);
        if (errno || *end || parsed == 0 || parsed > (uid_t)-1) {
            return false;
        }
        for (const char *p = name; *p; p++) {
            if (!((*p >= 'a' && *p <= 'z') || (*p >= 'A' && *p <= 'Z') ||
                  (*p >= '0' && *p <= '9') || strchr("_.-", *p))) {
                return false;
            }
        }
        if ((uid_t)parsed == uid && strcmp(name, bridge) == 0) {
            allowed = true;
        }
    }
    return allowed;
}

#ifdef EINK_BRIDGE_ACL_FILE
/* Walk the fixed policy path by descriptor, rejecting symlinks and writable
 * ancestors. The policy file itself must be a single root-owned regular file.
 * Linux ACL write grants are included in the group permission mask. */
static inline FILE *eink_bridge_acl_open(void)
{
    static const char *const parts[] = {"etc", "eink-qemu", "bridge", "bridge.conf"};
    struct stat st;
    int fd = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC);

    for (size_t i = 0; fd >= 0 && i < sizeof(parts) / sizeof(parts[0]); i++) {
        int child;
        bool leaf = i == sizeof(parts) / sizeof(parts[0]) - 1;

        if (fstat(fd, &st) || !S_ISDIR(st.st_mode) || st.st_uid != 0 ||
            (st.st_mode & 0022)) {
            close(fd);
            return NULL;
        }
        child = openat(fd, parts[i], O_RDONLY | O_NOFOLLOW | O_CLOEXEC |
                       (leaf ? O_NONBLOCK : O_DIRECTORY));
        close(fd);
        fd = child;
    }
    if (fd < 0) {
        return NULL;
    }
    if (fstat(fd, &st) || !S_ISREG(st.st_mode) || st.st_uid != 0 ||
        (st.st_mode & 0022) || st.st_nlink != 1 || st.st_size > 65536) {
        close(fd);
        return NULL;
    }
    FILE *file = fdopen(fd, "r");
    if (!file) {
        close(fd);
    }
    return file;
}
#endif
#endif
