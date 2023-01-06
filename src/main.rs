#![feature(hash_set_entry)]
#![feature(hash_raw_entry)]

mod parse;
mod sema;
mod util;

use crate::util::*;
use clap::*;
use std::path::Path;

/// Choice of output artifact

pub enum CompileTo {
  LLVMIr,
  Assembly,
  Object
}

fn compile(input_path: &Path, output_path: &Path, compile_to: CompileTo) -> MRes<()> {
  let repo = parse::parse_bundle(input_path)?;
  sema::compile(&repo, output_path, compile_to)
}

fn main() {
  util::init();

  let args = app_from_crate!()
    .arg(Arg::with_name("input")
      .help("Input file")
      .required(true)
      .index(1))
    .arg(Arg::with_name("assembly")
      .short("S")
      .help("Generate assembly"))
    .arg(Arg::with_name("llvm-ir")
      .short("L")
      .help("Generate LLVM IR"))
    .arg(Arg::with_name("output")
      .short("o")
      .long("output")
      .help("Output file")
      .required(true)
      .takes_value(true))
    .get_matches();

  let compile_to = if args.occurrences_of("llvm-ir") > 0 {
    CompileTo::LLVMIr
  } else if args.occurrences_of("assembly") > 0 {
    CompileTo::Assembly
  } else {
    CompileTo::Object
  };

  match compile(Path::new(args.value_of_os("input").unwrap()),
                  Path::new(args.value_of_os("output").unwrap()),
                  compile_to) {
    Ok(()) => eprintln!("ok :)"),
    Err(error) => eprintln!("{} :(", error),
  }

  util::uninit();
}
