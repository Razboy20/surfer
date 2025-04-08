use core::fmt;

/*
Architecture
~~~~~~~~~~~~

  - memory is byte addressable

  - words are 16 bits (little-endian)

  - 16 register names r0, r1, ..., r15 (16 bits each)

  - r0 is special:
      * reading from r0 always returns 0
      * writing to r0 interprets the the least significant
        8 bits as an ASCII code and prints that character

  - all instructions are 16 bit wide

  encoding          instruction   description

  0000aaaabbbbtttt  sub rt,ra,rb  regs[t] = regs[a] - regs[b]

  1000iiiiiiiitttt  movl rt,i     regs[t] = sign_extend(i)
  1001iiiiiiiitttt  movh rt,i     regs[t] = (regs[t] & 0xff) | (i << 8)

  1110aaaa0000tttt  jz rt,ra      pc = (regs[ra] == 0) ? regs[rt] : pc + 2
  1110aaaa0001tttt  jnz rt,ra     pc = (regs[ra] != 0) ? regs[rt] : pc + 2
  1110aaaa0010tttt  js rt,ra      pc = (regs[ra] < 0) ? regs[rt] : pc + 2
  1110aaaa0011tttt  jns rt,ra     pc = (regs[ra] >= 0) ? regs[rt] : pc + 2

  1111aaaa0000tttt  ld rt,ra      regs[t] = mem[regs[a]]
  1111aaaa0001tttt  st rt,ra      mem[regs[a]] = regs[t]
*/
pub enum I {
    SUB { rt: u8, ra: u8, rb: u8 },
    MOVL { rt: u8, i: u8 },
    MOVH { rt: u8, i: u8 },
    JZ { rt: u8, ra: u8 },
    JNZ { rt: u8, ra: u8 },
    JS { rt: u8, ra: u8 },
    JNS { rt: u8, ra: u8 },
    LD { rt: u8, ra: u8 },
    ST { rt: u8, ra: u8 },
}

impl fmt::Debug for I {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            I::SUB { rt, ra, rb } => write!(f, "sub r{:?}, r{:?}, r{:?}", rt, ra, rb),
            I::MOVL { rt, i } => write!(f, "movl r{:?}, #{:?}", rt, i),
            I::MOVH { rt, i } => write!(f, "movh r{:?}, #{:?}", rt, i),
            I::JZ { rt, ra } => write!(f, "jz r{:?}, r{:?}", rt, ra),
            I::JNZ { rt, ra } => write!(f, "jnz r{:?}, r{:?}", rt, ra),
            I::JS { rt, ra } => write!(f, "js r{:?}, r{:?}", rt, ra),
            I::JNS { rt, ra } => write!(f, "jns r{:?}, r{:?}", rt, ra),
            I::LD { rt, ra } => write!(f, "ld r{:?}, r{:?}", rt, ra),
            I::ST { rt, ra } => write!(f, "st r{:?}, r{:?}", rt, ra),
        }
    }
}

#[derive(Debug, Clone, Copy)]
/// Error types when converting `u16` to `I`
pub enum ConversionError {
    /// Unknown opcode
    UnknownOpcode(u16),
}

impl TryFrom<u16> for I {
    type Error = ConversionError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Ok(match (value & 0xF000) >> 12 {
            0b000 => I::SUB {
                rt: (value & 0xF) as u8,
                ra: ((value & 0xF00) >> 8) as u8,
                rb: ((value & 0xF0) >> 4) as u8,
            },
            0b1000 => I::MOVL {
                rt: (value & 0xF) as u8,
                i: ((value & 0xFF00) >> 8) as u8,
            },
            0b1001 => I::MOVH {
                rt: (value & 0xF) as u8,
                i: ((value & 0xFF00) >> 8) as u8,
            },
            0b1110 => match (value & 0xF0) >> 4 {
                0b0000 => I::JZ {
                    rt: (value & 0xF) as u8,
                    ra: ((value & 0xF00) >> 8) as u8,
                },
                0b0001 => I::JNZ {
                    rt: (value & 0xF) as u8,
                    ra: ((value & 0xF00) >> 8) as u8,
                },
                0b0010 => I::JS {
                    rt: (value & 0xF) as u8,
                    ra: ((value & 0xF00) >> 8) as u8,
                },
                0b0011 => I::JNS {
                    rt: (value & 0xF) as u8,
                    ra: ((value & 0xF00) >> 8) as u8,
                },
                _ => return Err(ConversionError::UnknownOpcode(value)),
            },
            0b1111 => match (value & 0xF0) >> 4 {
                0b0000 => I::LD {
                    rt: (value & 0xF) as u8,
                    ra: ((value & 0xF00) >> 8) as u8,
                },
                0b0001 => I::ST {
                    rt: (value & 0xF) as u8,
                    ra: ((value & 0xF00) >> 8) as u8,
                },
                _ => return Err(ConversionError::UnknownOpcode(value)),
            },
            _ => return Err(ConversionError::UnknownOpcode(value)),
        })
    }
}
