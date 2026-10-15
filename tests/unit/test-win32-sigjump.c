/* SPDX-License-Identifier: GPL-2.0-or-later */
/* TCG exits must work across generated frames, including x64-on-ARM Windows. */
#include "qemu/osdep.h"

static sigjmp_buf env;
static int jump_value;

static void jump_back(void)
{
    siglongjmp(env, jump_value);
}

static void test_generated_frame(void)
{
    /* Win64 shadow space, then call jump_back from code with no unwind table. */
    uint8_t code[] = {
        0x48, 0x83, 0xec, 0x28,
        0x48, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0,
        0xff, 0xd0,
        0x48, 0x83, 0xc4, 0x28, 0xc3,
    };
    uintptr_t target = (uintptr_t)jump_back;
    void *generated = VirtualAlloc(NULL, 4096, MEM_COMMIT | MEM_RESERVE,
                                   PAGE_READWRITE);
    DWORD old_protect;
    const int values[] = { 0, 1, 2, -2, -3 };

    g_assert_nonnull(generated);
    memcpy(code + 6, &target, sizeof(target));
    memcpy(generated, code, sizeof(code));
    g_assert_true(VirtualProtect(generated, 4096, PAGE_EXECUTE_READ,
                                &old_protect));
    g_assert_true(FlushInstructionCache(GetCurrentProcess(), generated,
                                       sizeof(code)));
    for (unsigned int i = 0; i < G_N_ELEMENTS(values); i++) {
        jump_value = values[i];
        int result = sigsetjmp(env, 0);
        if (result == 0) {
            ((void (*)(void))generated)();
            g_assert_not_reached();
        }
        g_assert_cmpint(result, ==, jump_value ? jump_value : 1);
    }
    g_assert_true(VirtualFree(generated, 0, MEM_RELEASE));
}

int main(int argc, char **argv)
{
    g_test_init(&argc, &argv, NULL);
    g_test_add_func("/win32/sigjump/generated-frame", test_generated_frame);
    return g_test_run();
}
