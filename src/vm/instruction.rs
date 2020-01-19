use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

pub enum InstructionKind {
    // JUMP
    Jump,
    EqJump,
    NeJump,
    // Data
    Move,
    Load,
    Store,
    // arith
    Add,
    Addu,
    Sub,
    Subu,
    And,
    Or,
    Xor,
    Nor,
    // compare
    Equal,
    Nequal,
    Lt,
    Gt,
    Le,
    Ge,
    Ltu,
    Gtu,
    Leu,
    Geu,
    // function
    Call,
    Ret,
}

pub fn write_move(reg_to: u8, reg_from: u8) -> u32{
    let mut inst:u32 = 0x0;
    inst |= 2 & 0x3f;
    inst |= ((reg_to as u32) << 6) & 0x3fc0;
    inst |= ((reg_from as u32) << 14) & 0x7fc000;
    inst |= (0 << 23) & 0xff800000;
    return inst;
}