/* Exercise cold and hot indirect branches across guest pages. */
.option norvc
.global _start
_start:
    li s1, 64
    jal s3, run
    li s1, 4000
    jal s3, run
    la t0, trap
    csrw mtvec, t0
    /* A locked, non-executable PMP entry forces a fault during TB lookup. */
    li t0, 0x18000000
    csrw pmpaddr0, t0
    li t0, 0x93
    csrw pmpcfg0, t0
    li t0, 0x60000000
    jalr zero, 0(t0)
done:
    li t0, 0x100000
    li t1, 0x5555
    sw t1, 0(t0)
1:  j 1b

run:
    li s2, 0
    la s0, targets
2:
    andi t0, s1, 1
    slli t0, t0, 2
    add t0, s0, t0
    lw t1, 0(t0)
    jalr ra, 0(t1)
    addi s1, s1, -1
    bnez s1, 2b
    /* This unrelated helper must retain the generic libffi path. */
    csrr t0, mhartid
    add s2, s2, t0
print:
    li t0, 0x10000000
    li t1, 8
3:
    srli t2, s2, 28
    li t3, 10
    bltu t2, t3, 4f
    addi t2, t2, 39
4:
    addi t2, t2, 48
    sb t2, 0(t0)
    slli s2, s2, 4
    addi t1, t1, -1
    bnez t1, 3b
    li t2, 10
    sb t2, 0(t0)
    jalr zero, 0(s3)

trap:
    csrr s2, mcause
    la s3, exception_pc
    j print
exception_pc:
    csrr s2, mepc
    la s3, done
    j print

.balign 4096
even:
    addi s2, s2, 3
    ret
.balign 4096
odd:
    addi s2, s2, 7
    ret
targets:
    .word even, odd
