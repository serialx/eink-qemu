#ifndef XTENSA_TRANSLATE_H
#define XTENSA_TRANSLATE_H

#include "cpu.h"
#include "exec/translator.h"
#include "tcg/tcg-op.h"

struct DisasContext {
    DisasContextBase base;
    const XtensaConfig *config;
    uint32_t pc;
    int cring;
    int ring;
    uint32_t lbeg_off;
    uint32_t lend;

    bool sar_5bit;
    bool sar_m32_5bit;
    TCGv_i32 sar_m32;

    unsigned window;
    unsigned callinc;
    bool cwoe;

    bool debug;
    bool icount;
    TCGv_i32 next_icount;

    unsigned cpenable;

    uint32_t op_flags;
    unsigned cost_extra;
    uint32_t cost_pc;
    xtensa_insnbuf_word insnbuf[MAX_INSNBUF_LENGTH];
    xtensa_insnbuf_word slotbuf[MAX_INSNBUF_LENGTH];
};

/* Shared translation helpers for Xtensa CPU extensions. */
extern TCGv_i32 cpu_SR[256];

void gen_exception_cause(DisasContext *dc, uint32_t cause);
MemOp gen_load_store_alignment(DisasContext *dc, MemOp mop, TCGv_i32 addr);
void get_f32_i1(const OpcodeArg *arg, OpcodeArg *arg32, int i0);
void put_f32_i1(const OpcodeArg *arg, const OpcodeArg *arg32, int i0);
void get_f32_o1(const OpcodeArg *arg, OpcodeArg *arg32, int o0);
void put_f32_o1(const OpcodeArg *arg, const OpcodeArg *arg32, int o0);

#endif /* XTENSA_TRANSLATE_H */
