

pub struct VM {
    registers: [u32; 32],
    equal_flag: bool,
    pc: usize,

    program: Vec<u8>,
    heap: Vec<u8>,
    stack: Vec<u8>,
}

impl VM {
    pub fn new() -> VM {
        VM {
            registers: [0; 32],
            equal_flag: false,

            program: vec![],
            pc: 0,

            heap: vec![],
            stack: vec![],
        }
    }
}