/* Stable TAP selection for managed Windows launches. */
#ifndef QEMU_TAP_WIN32_IDENTITY_H
#define QEMU_TAP_WIN32_IDENTITY_H

/* A GUID target cannot contain a path, option, or truncated adapter name.
 * Keep friendly-name selection as the legacy path. */
static inline bool tap_win32_guid_target(const char *target, char guid[39])
{
    const char *value;
    int i;

    if (!target || strncmp(target, "guid:", 5)) {
        return false;
    }
    value = target + 5;
    if (strlen(value) != 38 || value[0] != '{' || value[37] != '}') {
        return false;
    }
    for (i = 1; i < 37; ++i) {
        if (i == 9 || i == 14 || i == 19 || i == 24) {
            if (value[i] != '-') {
                return false;
            }
        } else if (!((value[i] >= '0' && value[i] <= '9') ||
                     (value[i] >= 'a' && value[i] <= 'f') ||
                     (value[i] >= 'A' && value[i] <= 'F'))) {
            return false;
        }
    }
    memcpy(guid, value, 39);
    return true;
}

#endif
