//! The scripted agent as a child process.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use common::{Link, Script};

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

fn parse_args() -> Result<Script, String> {
    let mut script = Script::default();
    for argument in std::env::args().skip(1) {
        if argument == "--fail" {
            script.fail = true;
        } else if argument == "--silent" {
            script.silent = true;
        } else if argument == "--rich" {
            script.rich = true;
        } else if argument == "--allow-always-only" {
            script.allow_always_only = true;
        } else if let Some(value) = argument.strip_prefix("--tool-kind=") {
            script.tool_kind = Some(value.to_owned());
        } else if let Some(value) = argument.strip_prefix("--record=") {
            script.record = Some(PathBuf::from(value));
        } else {
            return Err(format!("unknown argument {argument}"));
        }
    }
    Ok(script)
}

fn main() {
    let script = match parse_args() {
        Ok(script) => script,
        Err(message) => {
            eprintln!("mokmok-control-test-agent: {message}");
            std::process::exit(2);
        },
    };
    let mut link = Stdio::new();
    common::run_agent(&mut link, &script);
}
