/* GPSPI's live CMD word: warm cache, changes, PMP flush, timed completion. */
.option norvc
.section .text
.global _start
_start:
    li sp, 0x3fce0000
    li s0, 0x60024000
    li s1, 0x3fc90000
    li t0, 0x123
    sw t0, 0(s0)
    jal ra, reads
    sw a0, 0(s1)
    li t0, 0x800045 /* UPDATE clears, other bits remain visible. */
    sw t0, 0(s0)
    jal ra, reads
    sw a0, 4(s1)
    /* A PMP change invalidates the TLB including its live-word association. */
    li t0, -1
    csrw pmpaddr0, t0
    li t0, 0x0f /* TOR, R/W/X, unlocked: M mode remains unrestricted. */
    csrw pmpcfg0, t0
    li t0, 0x89
    sw t0, 0(s0)
    jal ra, reads
    sw a0, 8(s1)
    li t0, 0x80000000 /* APB 80 MHz, 512 bits = 6400 ns. */
    sw t0, 0x0c(s0)
    li t0, 511
    sw t0, 0x1c(s0)
    li t0, 0x01000000
    sw t0, 0(s0)
    li a0, 0
poll:
    /* A new TB must still get an icount I/O boundary despite a cache hit. */
    lw t1, 0(s0)
    addi a0, a0, 1
    and t1, t1, t0
    bnez t1, poll
    sw a0, 12(s1)
    lw t1, 0x3c(s0)
    sw t1, 16(s1)
    /* Deny the already cached word. A stale association must not hide a fault. */
    la t0, fault
    csrw mtvec, t0
    li t0, 0x18009000 /* CMD / 4: lower bound, entry 0 OFF. */
    csrw pmpaddr0, t0
    li t0, 0x18009001 /* upper bound: one word */
    csrw pmpaddr1, t0
    li t0, 0x8800 /* entry 1: locked TOR, no permissions, including M mode */
    csrw pmpcfg0, t0
    li t1, 0x76543210
    lw t1, 0(s0)
    sw t1, 24(s1)
    li t0, 0x1234abcd
    sw t0, 28(s1)
done:
    j done
reads:
    li a0, 0
    li t2, 5000
read_loop:
    lw t1, 0(s0)
    add a0, a0, t1
    addi t2, t2, -1
    bnez t2, read_loop
    ret

.balign 4
fault:
    csrr t2, mcause
    sw t2, 20(s1)
    csrr t2, mepc
    addi t2, t2, 4
    csrw mepc, t2
    mret
