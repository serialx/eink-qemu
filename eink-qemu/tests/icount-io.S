/* Repeated MMIO inside a TB, then modify its code and execute it again. */
.option norvc
.global _start
_start:
    li s3, 2
run:
    li s0, 0x10000005
    li s1, 0
    li s2, 2000
1:
    lbu a0, 0(s0)
accumulate:
    add s1, s1, a0
    addi s2, s2, -1
    bnez s2, 1b
    li s0, 0x10000000
    li s2, 4
2:
    sb s1, 0(s0)
    srli s1, s1, 8
    addi s2, s2, -1
    bnez s2, 2b
    addi s3, s3, -1
    la t2, accumulate
    li t3, 0x40a484b3  /* sub s1, s1, a0 */
    sw t3, 0(t2)
    fence.i
    bnez s3, run
    li t0, 0x100000
    li t1, 0x5555
    sw t1, 0(t0)
3:  j 3b
