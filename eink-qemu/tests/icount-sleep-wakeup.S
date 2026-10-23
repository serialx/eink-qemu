/* Measure the first timer wakeup after the host delays a sleeping vCPU. */
.option norvc
.global _start
_start:
    li s0, 0x10000000       /* virt UART */
    li s1, 0x0200bff8       /* CLINT mtime, 10 MHz */
    lw s2, 0(s1)
    li t0, 5000000         /* wake in 500 ms */
    add t0, t0, s2
    li t1, 0x02004000      /* hart 0 mtimecmp */
    li t2, -1
    sw t2, 4(t1)
    sw t0, 0(t1)
    sw zero, 4(t1)
    li t0, 128
    csrs mie, t0          /* wake WFI; leave global interrupts masked */
    li t0, 82             /* tell the test that the timer is armed */
    sb t0, 0(s0)
    wfi
    lw t0, 0(s1)
    sub t0, t0, s2
    li t1, 4
1:  sb t0, 0(s0)
    srli t0, t0, 8
    addi t1, t1, -1
    bnez t1, 1b
    li t0, 0x100000
    li t1, 0x5555
    sw t1, 0(t0)
2:  j 2b
