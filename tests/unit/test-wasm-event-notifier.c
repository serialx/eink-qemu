/* SPDX-License-Identifier: GPL-2.0-or-later */
/* Run with tests/unit/run-wasm-event-notifier.py after a Wasm build. */
#include "qemu/osdep.h"
#include "qemu/event_notifier.h"
#include <emscripten.h>
#include <pthread.h>
#include <stdatomic.h>
#include <poll.h>

static EventNotifier notifier;
static atomic_int queued, done, arm, gate, ready, held_ready, observed;
static atomic_long sets, clears, polls;
static pthread_mutex_t producer_lock = PTHREAD_MUTEX_INITIALIZER;

static void wait_for(atomic_int *flag)
{
    /* Deliberately remain in Wasm; no worker JS event loop or yielding. */
    while (!atomic_load(flag)) {
    }
}

static void set_notification(void)
{
    atomic_fetch_add(&sets, 1);
    assert(event_notifier_set(&notifier) == 0);
}

static int clear_notification(void)
{
    atomic_fetch_add(&clears, 1);
    return event_notifier_test_and_clear(&notifier);
}

static int probe(int timeout)
{
    struct pollfd fd = {
        .fd = event_notifier_get_fd(&notifier),
        .events = POLLIN,
    };
    int ret;

    atomic_fetch_add(&polls, 1);
    ret = poll(&fd, 1, timeout);
    assert(ret >= 0);
    return ret;
}

static void init_shared(void)
{
    assert(event_notifier_init(&notifier, false) == 0);
    event_notifier_enable_shared(&notifier);
    assert(notifier.shared_token);
}

static void delayed_wake(int token)
{
    /* Replay a transport callback after its notifier has been cleared/freed. */
    MAIN_THREAD_ASYNC_EM_ASM({
        const state = Module['qemuNotifierBridge'];
        const entry = state && state.entries.get($0);
        if (entry) {
            entry.stream.node.notifyListeners(1);
        }
    }, token);
}

static void *on_arm(void *unused)
{
    atomic_store(&ready, 1);
    wait_for(&arm);
    atomic_fetch_add(&queued, 1);
    set_notification();
    return NULL;
}

static void test_arm_race(bool after_registration)
{
    pthread_t producer;

    atomic_store(&ready, 0);
    atomic_store(&arm, 0);
    /* Force publication before or after addListener, after readiness derive. */
    MAIN_THREAD_EM_ASM({
        const node = FS.getStream($0).node;
        const original = node.addListener;
        node.addListener = function(cb) {
            let registration;
            if ($3) {
                registration = original.call(this, cb);
            }
            Atomics.store(HEAP32, Number($1) / 4, 1);
            while (!Atomics.load(HEAP32, Number($2) / 4)) {
            }
            node.addListener = original;
            return $3 ? registration : original.call(this, cb);
        };
    }, notifier.rfd, &arm, &notifier.shared_pending, after_registration);
    assert(pthread_create(&producer, NULL, on_arm, NULL) == 0);
    /* Starting the worker must not depend on the artificial JS spin above. */
    wait_for(&ready);
    assert(probe(5000) == 1);
    assert(pthread_join(producer, NULL) == 0);
    assert(clear_notification());
    assert(atomic_exchange(&queued, 0) == 1);
}

static void *holding_lock(void *unused)
{
    pthread_mutex_lock(&producer_lock);
    atomic_store(&held_ready, 1);
    atomic_fetch_add(&queued, 1);
    set_notification();
    /* Consumer must wake before we unlock or service any JS event loop. */
    wait_for(&observed);
    pthread_mutex_unlock(&producer_lock);
    return NULL;
}

static void test_producer_lock(void)
{
    pthread_t producer;

    assert(pthread_create(&producer, NULL, holding_lock, NULL) == 0);
    wait_for(&held_ready);
    assert(probe(5000) == 1);
    atomic_store(&observed, 1);
    pthread_mutex_lock(&producer_lock);
    pthread_mutex_unlock(&producer_lock);
    assert(pthread_join(producer, NULL) == 0);
    assert(clear_notification());
    assert(atomic_exchange(&queued, 0) == 1);
}

static void *burst_producer(void *opaque)
{
    intptr_t id = (intptr_t)opaque;

    wait_for(&gate);
    for (int i = 0; i < 10000; i++) {
        atomic_fetch_add(&queued, 1);
        set_notification();
        if ((i + id) % 31 == 0) {
            while (atomic_load(&queued)) {
            }
        }
    }
    atomic_fetch_add(&done, 1);
    set_notification();
    return NULL;
}

static void test_bursts(void)
{
    pthread_t producers[4];
    int consumed = 0;

    for (int i = 0; i < 4; i++) {
        assert(pthread_create(&producers[i], NULL, burst_producer,
                              (void *)(intptr_t)i) == 0);
    }
    atomic_store(&gate, 1);
    while (atomic_load(&done) != 4 || atomic_load(&queued)) {
        clear_notification();
        consumed += atomic_exchange(&queued, 0);
        if (atomic_load(&done) != 4 || atomic_load(&queued)) {
            /* A timeout cannot mask an indefinitely sleeping consumer. */
            int ready = probe(5000);
            assert(ready || !atomic_load(&queued));
        }
    }
    for (int i = 0; i < 4; i++) {
        assert(pthread_join(producers[i], NULL) == 0);
    }
    assert(consumed == 40000);
    clear_notification();
}

static void test_cleanup(void)
{
    int token = notifier.shared_token;
    uint64_t one = 1;

    delayed_wake(token);
    assert(!probe(2));
    MAIN_THREAD_EM_ASM({
        const entry = Module['qemuNotifierBridge'].entries.get($0);
        if (entry.stream.node.listeners && entry.stream.node.listeners.size) {
            throw Error('poll timeout leaked a listener');
        }
    }, token);
    event_notifier_cleanup(&notifier);
    delayed_wake(token);
    init_shared();
    assert(notifier.shared_token != token);
    delayed_wake(token);
    assert(!probe(2));
    event_notifier_cleanup(&notifier);

    /* External FD users retain the byte-carrying pipe and active init. */
    assert(event_notifier_init(&notifier, true) == 0);
    assert(!notifier.shared_token);
    assert(event_notifier_test_and_clear(&notifier));
    assert(!event_notifier_test_and_clear(&notifier));
    assert(write(event_notifier_get_wfd(&notifier), &one, sizeof(one)) ==
           sizeof(one));
    assert(event_notifier_test_and_clear(&notifier));
    event_notifier_cleanup(&notifier);
    event_notifier_cleanup(&notifier);
    assert(event_notifier_set(&notifier) == -1);
    assert(!event_notifier_test_and_clear(&notifier));
}

int main(void)
{
    init_shared();
    set_notification();
    set_notification();
    assert(probe(0) == 1);
    assert(clear_notification() == 1);
    assert(!clear_notification());
    assert(!probe(0));
    set_notification();
    assert(probe(0) == 1);
    assert(clear_notification());
    test_arm_race(false);
    test_arm_race(true);
    test_producer_lock();
    test_bursts();
    test_cleanup();
    printf("PASS queued=40000 sets=%ld clears=%ld polls=%ld\n",
           atomic_load(&sets), atomic_load(&clears), atomic_load(&polls));
    return 0;
}
