/* Developer probe; build on macOS with clang -framework vmnet.
 * No arguments inspects only. `eligible en0` checks native eligibility.
 * `open PATH` requires an ordinary user and verifies QEMU socket framing
 * between two clients of the actual packet service, without Internet traffic.
 */
#include <arpa/inet.h>
#include <errno.h>
#include <grp.h>
#include <pwd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/sysctl.h>
#include <sys/un.h>
#include <unistd.h>
#include <vmnet/vmnet.h>
#include <xpc/xpc.h>

static void json_string(const char *s) {
    putchar('"');
    for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
        if (*p == '"' || *p == '\\') { putchar('\\'); putchar(*p); }
        else if (*p < 32) printf("\\u%04x", *p);
        else putchar(*p);
    }
    putchar('"');
}
static void property(const char *key) {
    char value[256] = {0}; size_t size = sizeof(value) - 1;
    if (sysctlbyname(key, value, &size, NULL, 0) != 0) strcpy(value, "unknown");
    json_string(value);
}
static int connect_socket(const char *path) {
    struct sockaddr_un address = {.sun_family = AF_UNIX};
    if (strlen(path) >= sizeof(address.sun_path)) { errno = ENAMETOOLONG; return -1; }
    strcpy(address.sun_path, path);
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    struct timeval timeout = {.tv_sec = 5};
    if (setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout)) ||
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout)) ||
        connect(fd, (struct sockaddr *)&address, sizeof(address))) {
        int saved = errno; close(fd); errno = saved; return -1;
    }
    return fd;
}
static int transfer(int fd, void *buffer, size_t size, int sending) {
    size_t done = 0;
    while (done < size) {
        ssize_t n = sending ? write(fd, (char *)buffer + done, size - done) : read(fd, (char *)buffer + done, size - done);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) return -1;
        done += (size_t)n;
    }
    return 0;
}
static int open_as_user(const char *path) {
    if (getuid() == 0 || geteuid() == 0) { fputs("Packet verification requires an ordinary user.\n", stderr); return 1; }
    alarm(10);
    int receiver = connect_socket(path);
    if (receiver < 0) { perror("connect receiver"); return 1; }
    int sender = connect_socket(path);
    if (sender < 0) { perror("connect sender"); close(receiver); return 1; }
    unsigned char packet[64] = {0};
    uint32_t length = htonl(60);
    memcpy(packet, &length, 4);
    memset(packet + 4, 0xff, 6); /* broadcast */
    packet[10] = 0x02; packet[15] = 0x01; /* locally administered source */
    packet[16] = 0x88; packet[17] = 0xb5; /* local experimental EtherType */
    arc4random_buf(packet + 18, sizeof(packet) - 18);
    int status = transfer(sender, packet, sizeof(packet), 1);
    unsigned char received[65536];
    int matched = 0;
    /* Other broadcasts may arrive first. The alarm bounds the entire check. */
    for (unsigned i = 0; status == 0 && i < 128; i++) {
        uint32_t header;
        status = transfer(receiver, &header, sizeof(header), 0);
        if (status) break;
        size_t count = ntohl(header);
        if (count < 14 || count > sizeof(received)) { status = -1; break; }
        status = transfer(receiver, received, count, 0);
        if (!status && count == 60 && !memcmp(received, packet + 4, 60)) { matched = 1; break; }
    }
    close(sender); close(receiver);
    if (!matched) { fputs("Packet exchange failed.\n", stderr); return 1; }
    printf("{\"openAsUser\":\"passed\",\"uid\":%u,\"euid\":%u,\"frameBytes\":60}\n", getuid(), geteuid());
    return 0;
}
/* Test-only identity drop. No arbitrary command is accepted and no identity
 * can be changed unless the developer already launched this probe as root. */
static int deny_as(const char *user, const char *group, const char *path) {
    if (geteuid() != 0) return 77;
    struct passwd *pw = getpwnam(user);
    if (!pw || pw->pw_uid == 0) return 2;
    uid_t uid = pw->pw_uid;
    struct group *gr = getgrnam(group);
    if (!gr) return 2;
    gid_t gid = gr->gr_gid;
    if (setgroups(1, &gid) || setgid(gid) || setuid(uid) ||
        getuid() != uid || geteuid() != uid || getgid() != gid || getegid() != gid) {
        perror("drop identity"); return 1;
    }
    int fd = connect_socket(path);
    if (fd >= 0) { close(fd); fputs("Other user unexpectedly opened socket\n", stderr); return 1; }
    if (errno != EACCES) { perror("unexpected denial error"); return 1; }
    printf("{\"otherUserDenied\":true,\"uid\":%u,\"euid\":%u,\"gid\":%u,\"errno\":%d}\n", getuid(), geteuid(), getgid(), errno);
    return 0;
}
int main(int argc, char **argv) {
    if (argc == 5 && !strcmp(argv[1], "deny-as")) return deny_as(argv[2], argv[3], argv[4]);
    if (argc == 3 && !strcmp(argv[1], "open")) return open_as_user(argv[2]);
    int eligibility = argc == 3 && !strcmp(argv[1], "eligible");
    if (argc != 1 && !eligibility) { fputs("Usage: probe-macos [eligible INTERFACE | open SOCKET]\n", stderr); return 2; }
    xpc_object_t list = vmnet_copy_shared_interface_list();
    if (!list) { fputs("vmnet inspection failed\n", stderr); return 1; }
    int found = 0;
    if (!eligibility) {
        printf("{\"os\":\"macOS\",\"release\":"); property("kern.osproductversion");
        printf(",\"build\":"); property("kern.osversion");
        printf(",\"hardware\":"); property("hw.model");
#if defined(__arm64__)
        printf(",\"architecture\":\"arm64\"");
#else
        printf(",\"architecture\":\"x86_64\"");
#endif
        printf(",\"inspect\":\"passed\",\"install\":\"untested\",\"openAsUser\":\"untested\",\"restore\":\"untested\",\"adapters\":[");
    }
    for (size_t i = 0; i < xpc_array_get_count(list); i++) {
        const char *name = xpc_array_get_string(list, i);
        if (!name) continue;
        if (eligibility) { if (!strcmp(name, argv[2])) found = 1; }
        else { if (found) putchar(','); json_string(name); found = 1; }
    }
    xpc_release(list);
    if (!eligibility) { puts("]}"); return 0; }
    return found ? 0 : 1;
}
