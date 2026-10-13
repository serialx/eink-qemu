/* Alternate the same hot load between RAM and MMIO, then cross a page. */
.option norvc
.global _start
_start:
    la t0, trap
    csrw mtvec, t0
    li s0, 0x80400000
    li t0, 0x60
    sb t0, 0(s0)
    /* Cold signed/unsigned byte and halfword loads, and narrow stores. */
    li t0, 0x89abcdef
    sw t0, 8(s0)
    lw t1, 8(s0)
    bne t1, t0, fail
    lb t1, 8(s0)
    li t2, -17
    bne t1, t2, fail
    lbu t1, 8(s0)
    li t2, 239
    bne t1, t2, fail
    lh t1, 8(s0)
    li t2, -12817
    bne t1, t2, fail
    lhu t1, 8(s0)
    li t2, 52719
    bne t1, t2, fail
    li t1, 0x7654
    sh t1, 8(s0)
    li t1, 0x12
    sb t1, 10(s0)
    lw t1, 8(s0)
    li t2, 0x89127654
    bne t1, t2, fail
    li s1, 64
    jal run
    li s1, 8000
    jal run
    /* Invalidate an already hot code page through a guest store. */
    li s1, 4000
5:
    jal mutated
    li t0, 1
    bne a0, t0, fail
    addi s1, s1, -1
    bnez s1, 5b
    la t0, mutated
    li t1, 0x00200513 /* addi a0, zero, 2 */
    sw t1, 0(t0)
    fence.i
    jal mutated
    li t0, 2
    bne a0, t0, fail
    /* Deny a data page after warming the load TB. */
    li t0, 0x20100000
    csrw pmpaddr0, t0
    li t0, 0x20100400
    csrw pmpaddr1, t0
    li t0, 0x8800
    csrw pmpcfg0, t0
    li s4, 0
    lw t0, 0(s0)
    li t0, 1
    bne s4, t0, fail
    li t0, 0x10000000
    li t1, 79
    sb t1, 0(t0)
    li t1, 75
    sb t1, 0(t0)
    li t1, 10
    sb t1, 0(t0)
    li t0, 0x100000
    li t1, 0x5555
    sw t1, 0(t0)
1:  j 1b
run:
    li s2, 0
    li s3, 0
2:
    mv t0, s0
    andi t1, s1, 1
    beqz t1, 3f
    li t0, 0x10000005
3:
    lbu t1, 0(t0)
    add s2, s2, t1
    addi s3, s3, 0x60
    /* Keep another RAM access after the possibly slow load. */
    sw s2, 4(s0)
    lw t2, 4(s0)
    bne t2, s3, fail
    /* Unaligned word straddling a page uses the slow path. */
    li t0, 0x80401ffe
    sw s2, 0(t0)
    lw t2, 0(t0)
    bne t2, s2, fail
    addi s1, s1, -1
    bnez s1, 2b
    ret
trap:
    csrr t0, mcause
    li t1, 5
    bne t0, t1, fail
    csrr t0, mtval
    bne t0, s0, fail
    addi s4, s4, 1
    csrr t0, mepc
    addi t0, t0, 4
    csrw mepc, t0
    mret
fail:
    li t0, 0x100000
    li t1, 0x13333
    sw t1, 0(t0)
4:  j 4b

.balign 4096
mutated:
    addi a0, zero, 1
    ret
