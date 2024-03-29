/*
 * Copyright (c) 2022-2023, Mate Kukri
 * SPDX-License-Identifier: GPL-2.0-only
 */

#![feature(hash_set_entry)]
#![feature(hash_raw_entry)]

mod parse;
mod resolve;
mod sema;
mod lower;
pub mod util;

use crate::util::*;
use std::path::Path;

/// Choice of output artifact

pub enum CompileTo {
  LLVMIr,
  Assembly,
  Object
}

pub fn compile(input_path: &Path, output_path: &Path, compile_to: CompileTo, triple: Option<&str>) -> MRes<()> {
  let parsed_repo = parse::parse_bundle(input_path)?;
  let mut inst_collection = sema::analyze(&parsed_repo)?;
  lower::compile(&mut inst_collection, output_path, compile_to, triple)
}
