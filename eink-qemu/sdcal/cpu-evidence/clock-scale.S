.option norvc
.global _start
_start:
 li s0, 0x600c0000
 li t0, 1
 sw t0, 8(s0)
 li t0, 0xa8400
 sw t0, 0x58(s0)
 la s2, results
 jal ra, measure
 sw a0, 0(s2)
 li t0, 0xa8003
 sw t0, 0x58(s0)
 jal ra, measure
 sw a0, 4(s2)
 li t0, 1
 sw t0, 8(s2)
1: wfi
 j 1b
measure:
 li s1, 100000
 rdcycle t2
2: addi s1, s1, -1
 bnez s1, 2b
 rdcycle t3
 sub a0, t3, t2
 ret
.data
.global results
results: .zero 12
