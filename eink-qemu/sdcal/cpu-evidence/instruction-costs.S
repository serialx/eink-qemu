.option norvc
.global _start
_start:
 li s0, 0x200bff8
 la s1, results
 la s2, data
 mv a1, s2
 li s3, 37
 li s4, 3
.macro bench op:vararg
 li t0, 10000
 lw t2, 0(s0)
1:
 \op
 addi t0, t0, -1
 bnez t0, 1b
 lw t3, 0(s0)
 sub t3, t3, t2
 sw t3, 0(s1)
 addi s1, s1, 4
.endm
 bench add a0, s3, s4
 bench lw a0, 0(s2)
 bench sw a0, 0(s2)
 bench mul a0, s3, s4
 bench div a0, s3, s4
 bench rem a0, s3, s4
.option rvc
 bench c.lw a0, 0(a1)
 bench c.sw a0, 0(a1)
.option norvc
 la s1, results
 li t0, 32
 li s2, 0x10000000
2:
 lbu a0, 0(s1)
 sb a0, 0(s2)
 addi s1, s1, 1
 addi t0, t0, -1
 bnez t0, 2b
 li t0, 0x100000
 li t1, 0x5555
 sw t1, 0(t0)
3: j 3b
.data
.align 2
data: .word 0
results: .zero 32
