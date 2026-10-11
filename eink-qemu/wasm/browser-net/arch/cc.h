#ifndef X4_BROWSER_CC_H
#define X4_BROWSER_CC_H
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#define LWIP_PLATFORM_DIAG(x) do { printf x; } while (0)
#define LWIP_PLATFORM_ASSERT(x) do { fprintf(stderr, "%s\n", x); abort(); } while (0)
uint32_t browser_random(void);
#define LWIP_RAND() browser_random()
#endif
