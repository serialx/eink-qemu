/* The app-managed privileged helper uses only its installer-owned ACL.
 * The ordinary QEMU helper retains executable-relative relocation. */
#ifndef QEMU_BRIDGE_HELPER_PATH_H
#define QEMU_BRIDGE_HELPER_PATH_H

static inline char *bridge_acl_path(void)
{
#ifdef EINK_BRIDGE_ACL_FILE
    return g_strdup(EINK_BRIDGE_ACL_FILE);
#else
    return get_relocated_path(DEFAULT_ACL_FILE);
#endif
}

#endif
