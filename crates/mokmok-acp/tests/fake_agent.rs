//! The scripted agent as a child process.

mod common;

use std::io::{BufRead, BufReader, Write};

use common::Link;

struct Stdio {
    input: BufReader<std::io::Stdin>,
    output: std::io::Stdout,
}

impl Stdio {
    fn new() -> Self {
        Self {
            input: BufReader::new(std::io::stdin()),
            output: std::io::stdout(),
        }
    }
}

impl Link for Stdio {
    fn send(
        &mut self,
        message: &str,
    ) {
        let _ = writeln!(self.output, "{message}");
        let _ = self.output.flush();
    }

    fn recv(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.input.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end().to_owned()),
        }
    }
}

fn main() {
    let mut link = Stdio::new();
    common::run_agent(&mut link);
}
