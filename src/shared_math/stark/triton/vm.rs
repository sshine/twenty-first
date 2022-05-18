use super::instruction::{parse, Instruction};
use super::state::VMState;
use crate::shared_math::rescue_prime_xlix;
use std::error::Error;
use std::fmt::Display;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub instructions: Vec<Instruction>,
}

impl Display for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.instructions
            .iter()
            .fold(Ok(()), |result, instruction| {
                result.and_then(|()| writeln!(f, "{}", instruction))
            })
    }
}

impl Program {
    pub fn from_code(code: &str) -> Result<Self, Box<dyn Error>> {
        let instructions = parse(code)?;
        Ok(Program::from_instr(&instructions))
    }

    pub fn from_instr(input: &[Instruction]) -> Self {
        let mut instructions = vec![];
        for instr in input {
            instructions.append(&mut vec![*instr; instr.size()]);
        }
        Program { instructions }
    }
}

#[allow(clippy::needless_lifetimes)]
pub fn run<'pgm>(program: &'pgm Program) -> Result<Vec<VMState<'pgm>>, Box<dyn Error>> {
    let mut rng = rand::thread_rng();
    let rescue_prime = rescue_prime_xlix::neptune_params();
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut trace = vec![VMState::new(&program.instructions)];
    while !trace.last().unwrap().is_final() {
        let derp1 = trace.last().unwrap();
        let derp2 = derp1.step(&mut rng, &rescue_prime, &mut stdin, &mut stdout);
        if derp2.is_err() {
            for x in trace.iter() {
                println!("{}", x);
            }
        }
        trace.push(derp2?);
    }

    Ok(trace)
}

#[cfg(test)]
mod triton_vm_tests {
    use super::Instruction::*;
    use super::*;

    #[test]
    fn vm_run_test() {
        let instructions = vec![Push(2.into()), Push(2.into()), Add];
        let program = Program { instructions };
        let _empty_run = run(&program);
    }
}
