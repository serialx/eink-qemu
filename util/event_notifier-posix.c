/*
 * event notifier support
 *
 * Copyright Red Hat, Inc. 2010
 *
 * Authors:
 *  Michael S. Tsirkin <mst@redhat.com>
 *
 * This work is licensed under the terms of the GNU GPL, version 2 or later.
 * See the COPYING file in the top-level directory.
 */

#include "qemu/osdep.h"
#include "qapi/error.h"
#include "qemu/cutils.h"
#include "qemu/event_notifier.h"
#include "qemu/main-loop.h"

#ifdef __EMSCRIPTEN__
#include <emscripten.h>
#include "qemu/atomic.h"

/*
 * Keep a real pipe FD for GLib/AIO poll integration, but publish private
 * notifier readiness in shared memory. The JS poll callback reads it with
 * acquire ordering; set/clear use full-barrier exchanges. Existing AIO
 * notify_me/notified barriers still publish work and arm/recheck the consumer.
 *
 * A 0->1 transition posts a one-shot asynchronous JS-main listener wake.
 * The producer never waits for that callback and the consumer need not run
 * its worker event loop: the callback resolves its outstanding host poll.
 * JS readiness derivation and listener registration execute in one turn;
 * a racing set is either observed there or its queued callback wakes the
 * newly registered listener. Clear cannot overwrite a later concurrent set.
 *
 * Cleanup requires quiescent producers, as for the ordinary notifier. Tokens
 * are not reused, so delayed callbacks cannot access a freed notifier or an
 * unrelated descriptor after close. Unsupported runtimes retain the pipe.
 */
static int event_notifier_shared_init(int fd, unsigned *pending)
{
    return MAIN_THREAD_EM_ASM_INT({
        const stream = FS.getStream($0);
        if (!stream || !stream.node.addListener ||
            !stream.node.notifyListeners || !stream.stream_ops.poll) {
            return 0;
        }
        let state = Module['qemuNotifierBridge'];
        if (!state) {
            state = {};
            state.next = 1;
            state.entries = new Map();
            Module['qemuNotifierBridge'] = state;
        }
        if (state.next > 2147483647) {
            return 0;
        }
        const token = state.next++;
        const index = Number($1) / 4;
        const original = stream.stream_ops;
        stream.stream_ops = Object.assign({}, original);
        stream.stream_ops.poll = function(s) {
            return original.poll(s) | (Atomics.load(HEAP32, index) ? 1 : 0);
        };
        const entry = {};
        entry.stream = stream;
        entry.original = original;
        state.entries.set(token, entry);
        return token;
    }, fd, pending);
}

static void event_notifier_shared_wake(int token)
{
    MAIN_THREAD_ASYNC_EM_ASM({
        const state = Module['qemuNotifierBridge'];
        const entry = state && state.entries.get($0);
        if (entry) {
            entry.stream.node.notifyListeners(1);
        }
    }, token);
}

static void event_notifier_shared_cleanup(int token)
{
    MAIN_THREAD_EM_ASM({
        const state = Module['qemuNotifierBridge'];
        const entry = state && state.entries.get($0);
        if (entry) {
            entry.stream.stream_ops = entry.original;
            state.entries.delete($0);
        }
    }, token);
}

void event_notifier_enable_shared(EventNotifier *e)
{
    assert(e->initialized && !e->shared_token);
    e->shared_token = event_notifier_shared_init(e->rfd, &e->shared_pending);
}
#endif

#ifdef CONFIG_EVENTFD
#include <sys/eventfd.h>
#endif

#ifdef CONFIG_EVENTFD
/*
 * Initialize @e with existing file descriptor @fd.
 * @fd must be a genuine eventfd object, emulation with pipe won't do.
 */
void event_notifier_init_fd(EventNotifier *e, int fd)
{
    e->rfd = fd;
    e->wfd = fd;
#ifdef __EMSCRIPTEN__
    e->shared_pending = 0;
    e->shared_token = 0;
#endif
    e->initialized = true;
}
#endif

int event_notifier_init(EventNotifier *e, int active)
{
    int fds[2];
    int ret;
    Error *local_err = NULL;

#ifdef CONFIG_EVENTFD
    ret = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
#else
    ret = -1;
    errno = ENOSYS;
#endif
    if (ret >= 0) {
        e->rfd = e->wfd = ret;
    } else {
        if (errno != ENOSYS) {
            return -errno;
        }
        if (!g_unix_open_pipe(fds, FD_CLOEXEC, NULL)) {
            return -errno;
        }
        if (!qemu_set_blocking(fds[0], false, &local_err)) {
            ret = -errno;
            goto fail;
        }
        if (!qemu_set_blocking(fds[1], false, &local_err)) {
            ret = -errno;
            goto fail;
        }
        e->rfd = fds[0];
        e->wfd = fds[1];
    }
#ifdef __EMSCRIPTEN__
    e->shared_pending = 0;
    e->shared_token = 0;
#endif
    e->initialized = true;
    if (active) {
        event_notifier_set(e);
    }
    return 0;

fail:
    error_report_err(local_err);
    close(fds[0]);
    close(fds[1]);
    return ret;
}

void event_notifier_cleanup(EventNotifier *e)
{
    if (!e->initialized) {
        return;
    }

#ifdef __EMSCRIPTEN__
    if (e->shared_token) {
        event_notifier_shared_cleanup(e->shared_token);
        e->shared_token = 0;
    }
#endif
    if (e->rfd != e->wfd) {
        close(e->rfd);
    }

    e->rfd = -1;
    close(e->wfd);
    e->wfd = -1;
    e->initialized = false;
}

int event_notifier_get_fd(const EventNotifier *e)
{
    return e->rfd;
}

int event_notifier_get_wfd(const EventNotifier *e)
{
    return e->wfd;
}

int event_notifier_set(EventNotifier *e)
{
    static const uint64_t value = 1;
    ssize_t ret;

    if (!e->initialized) {
        return -1;
    }

#ifdef __EMSCRIPTEN__
    if (e->shared_token) {
        if (!qatomic_xchg(&e->shared_pending, 1)) {
            event_notifier_shared_wake(e->shared_token);
        }
        return 0;
    }
#endif
    do {
        ret = write(e->wfd, &value, sizeof(value));
    } while (ret < 0 && errno == EINTR);

    /* EAGAIN is fine, a read must be pending.  */
    if (ret < 0 && errno != EAGAIN) {
        return -errno;
    }
    return 0;
}

int event_notifier_test_and_clear(EventNotifier *e)
{
    int value;
    ssize_t len;
    char buffer[512];

    if (!e->initialized) {
        return 0;
    }

#ifdef __EMSCRIPTEN__
    if (e->shared_token) {
        return qatomic_xchg(&e->shared_pending, 0);
    }
#endif
    /* Drain the notify pipe.  For eventfd, only 8 bytes will be read.  */
    value = 0;
    do {
        len = read(e->rfd, buffer, sizeof(buffer));
        value |= (len > 0);
    } while ((len == -1 && errno == EINTR) || len == sizeof(buffer));

    return value;
}
