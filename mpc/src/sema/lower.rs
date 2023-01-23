/*
 * Copyright (c) 2022-2023, Mate Kukri
 * SPDX-License-Identifier: GPL-2.0-only
 */

use super::*;
use llvm_sys::core::*;
use llvm_sys::LLVMIntPredicate::*;
use llvm_sys::LLVMRealPredicate::*;
use llvm_sys::prelude::*;
use llvm_sys::target::*;
use llvm_sys::target_machine::*;

type BB = LLVMBasicBlockRef;
type Val = LLVMValueRef;

/// Semantics of a type
enum Semantics {
  Void,
  Value,
  Addr
}

/// Lower a constant value into an LLVM constant expression
unsafe fn lower_const_val(val: &ConstVal, ctx: &mut LowerCtx) -> Val {
  use ConstVal::*;
  match val {
    FuncPtr { id } => ctx.get_value(id),
    DataPtr { ptr } => lower_const_ptr(ptr, ctx),
    BoolLit { val } => ctx.build_bool(*val),
    IntLit { ty, val } => ctx.build_int(ty, *val as usize),
    FltLit { ty, val } => ctx.build_flt(ty, *val),
    ArrLit { vals, .. } |
    StructLit { vals, .. } => {
      let mut vals: Vec<Val> = vals
        .iter()
        .map(|val| lower_const_val(val, ctx))
        .collect();
      let types: Vec<LLVMTypeRef> =
        vals.iter().map(|value| LLVMTypeOf(*value)).collect();
      LLVMConstNamedStruct(
        ctx.lower_anon_struct(&types),
        vals.as_mut_ptr() as _,
        vals.len() as _)
    }
    UnionLit { ty, val, .. } => {
      let l_type = ctx.lower_ty(ty);
      let l_val = lower_const_val(val, ctx);
      let mut vals = [
        l_val, // Value
        LLVMConstNull(LLVMArrayType(
          LLVMInt8TypeInContext(ctx.l_context),
          (ctx.size_of(l_type) - ctx.size_of(LLVMTypeOf(l_val))) as _))
      ];
      let types: Vec<LLVMTypeRef> =
        vals.iter().map(|value| LLVMTypeOf(*value)).collect();
      LLVMConstNamedStruct(
        ctx.lower_anon_struct(&types),
        vals.as_mut_ptr() as _,
        vals.len() as _)
    }
    CStrLit { val } => {
      ctx.build_string_lit(val)
    }
  }
}

/// Predict the **LLVM** type of the constant expression returned by the above
unsafe fn const_init_ty(val: &ConstVal, ctx: &mut LowerCtx) -> LLVMTypeRef {
  use ConstVal::*;
  match val {
    FuncPtr { .. } |
    DataPtr { .. } |
    CStrLit { .. } => LLVMPointerTypeInContext(ctx.l_context, 0),

    BoolLit { .. } => LLVMInt1TypeInContext(ctx.l_context),

    IntLit { ty, .. } |
    FltLit { ty, .. } => ctx.lower_ty(ty),

    ArrLit { vals, .. } |
    StructLit { vals, .. } => {
      let l_types: Vec<LLVMTypeRef> = vals
        .iter()
        .map(|val| const_init_ty(val, ctx))
        .collect();

      ctx.lower_anon_struct(&l_types)
    }

    UnionLit { ty, val, .. } => {
      let l_union_type = ctx.lower_ty(ty);
      let union_size = ctx.size_of(l_union_type);

      let l_val_type = const_init_ty(val, ctx);

      let l_types = [
        l_val_type, // Value
        LLVMArrayType(LLVMInt8TypeInContext(ctx.l_context),
                      (union_size - ctx.size_of(l_val_type)) as _)  // Padding
      ];

      ctx.lower_anon_struct(&l_types)
    }
  }
}

/// Lower a constant ptr to an LLVM constant pointer
unsafe fn lower_const_ptr(ptr: &ConstPtr, ctx: &mut LowerCtx) -> Val {
  match ptr {
    ConstPtr::Data { id, ..} => ctx.get_value(&(*id, vec![])),
    ConstPtr::StrLit { val, ..  } => ctx.build_string_lit(val),
    ConstPtr::ArrayElement { base, idx, .. } |
    ConstPtr::StructField { base, idx, .. } => {
      let l_ptr = lower_const_ptr(base, ctx);
      ctx.build_const_gep(base.ty(), l_ptr, *idx)
    }
    ConstPtr::UnionField { base, .. } => {
      lower_const_ptr(base, ctx)
    }
  }
}

/// Expressions

unsafe fn lower_lvalue(lvalue: &LValue, ctx: &mut LowerCtx) -> Val {
  match lvalue {
    LValue::DataRef { id, .. } => {
      ctx.get_value(&(*id, vec![]))
    }
    LValue::ParamRef { index, .. } => {
      ctx.params[*index]
    }
    LValue::LetRef { index, .. } => {
      ctx.locals[*index]
    }
    LValue::BindingRef { index, .. } => {
      ctx.bindings[*index]
    }
    LValue::StrLit { val, .. } => {
      ctx.build_string_lit(val)
    }
    LValue::ArrayLit { ty, elements, .. } => {
      let l_storage = ctx.allocate_local(ty);
      let elements: Vec<(Ty, LLVMValueRef)> = elements.iter()
        .map(|element| (element.ty().clone(), lower_rvalue(element, ctx)))
        .collect();
      ctx.build_aggregate_inplace(ty, l_storage, &elements);
      l_storage
    }
    LValue::UnionLit { ty, val, .. } => {
      let l_storage = ctx.allocate_local(ty);
      let l_val = lower_rvalue(val, ctx);
      ctx.build_store(val.ty(), l_storage, l_val);
      l_storage
    }
    LValue::StructLit { ty, fields, .. } => {
      let l_storage = ctx.allocate_local(ty);
      let fields: Vec<(Ty, LLVMValueRef)> = fields.iter()
        .map(|field| (field.ty().clone(), lower_rvalue(field, ctx)))
        .collect();
      ctx.build_aggregate_inplace(ty, l_storage, &fields);
      l_storage
    }
    LValue::UnitVariantLit { ty, index, .. } => {
      let l_storage = ctx.allocate_local(ty);
      // Write tag
      let l_tag = ctx.build_int(&Ty::Int32, *index);
      ctx.build_store(&Ty::Int32, l_storage, l_tag);
      l_storage
    }
    LValue::StructVariantLit { ty, index, fields, .. } => {
      let l_storage = ctx.allocate_local(ty);
      // Write tag
      let l_tag = ctx.build_int(&Ty::Int32, *index);
      ctx.build_store(&Ty::Int32, l_storage, l_tag);

      // Write data
      let l_dest = ctx.build_gep(ty, l_storage, 1);
      let l_fields: Vec<(Ty, LLVMValueRef)> = fields.iter()
        .map(|field| (field.ty().clone(), lower_rvalue(field, ctx)))
        .collect();
      // NOTE: this is kind of hacky, we should be storing the pre-computed variant types
      //       during enum lowering
      let variant_ty = Ty::Tuple(l_fields
        .iter()
        .map(|(ty, _)| (RefStr::new(""), ty.clone()))
        .collect());
      ctx.build_aggregate_inplace(&variant_ty, l_dest, &l_fields);

      l_storage
    }
    LValue::StruDot { arg, idx, .. } => {
      let l_ptr = lower_lvalue(arg, ctx);
      ctx.build_gep(arg.ty(), l_ptr, *idx)
    }
    LValue::UnionDot { arg, .. } => {
      lower_lvalue(arg, ctx)
    }
    LValue::Index { arg, idx, .. } => {
      let l_ptr = lower_lvalue(arg, ctx);
      let l_idx = lower_rvalue(idx, ctx);
      ctx.build_index(arg.ty(), l_ptr, l_idx)
    }
    LValue::Ind { arg, .. } => {
      lower_rvalue(arg, ctx)
    }
  }
}

unsafe fn lower_rvalue(rvalue: &RValue, ctx: &mut LowerCtx) -> Val {
  match rvalue {
    RValue::Unit { .. } => {
      ctx.build_void()
    }
    RValue::FuncRef { id, .. } => {
      ctx.get_value(id)
    }
    RValue::CStr { val, .. } => {
      ctx.build_string_lit(val)
    }
    RValue::Load { ty, arg, .. } => {
      let addr = lower_lvalue(arg, ctx);
      ctx.build_load(ty, addr)
    }
    RValue::Nil { ty, .. } => {
      LLVMConstNull(ctx.lower_ty(ty))
    }
    RValue::Bool { val, .. } => {
      ctx.build_bool(*val)
    }
    RValue::Int { ty, val, .. } => {
      ctx.build_int(ty, *val)
    }
    RValue::Flt { ty, val, .. } => {
      ctx.build_flt(ty, *val)
    }
    RValue::Call { arg, args, .. } => {
      let l_func = lower_rvalue(arg, ctx);
      let l_args = args.iter()
        .map(|arg| lower_rvalue(arg, ctx))
        .collect();
      ctx.build_call(arg.ty(), l_func, l_args)
    }
    RValue::Adr { arg, .. } => {
      lower_lvalue(arg, ctx)
    }
    RValue::Un { op, arg, .. } => {
      let l_arg = lower_rvalue(arg, ctx);
      ctx.build_un(arg.ty(), *op, l_arg)
    }
    RValue::Cast { ty, arg } => {
      let l_arg = lower_rvalue(arg, ctx);
      ctx.build_cast(ty, arg.ty(), l_arg)
    }
    RValue::Bin { op, lhs, rhs, .. } => {
      let l_lhs = lower_rvalue(lhs, ctx);
      let l_rhs = lower_rvalue(rhs, ctx);
      ctx.build_bin(lhs.ty(), *op, l_lhs, l_rhs)
    }
    RValue::LNot { .. } |
    RValue::LAnd { .. } |
    RValue::LOr { .. } => {
      // Split based on the boolean value
      let true_block = ctx.new_block();
      let false_block = ctx.new_block();
      lower_bool(rvalue, ctx, true_block, false_block);

      // Both paths will merge in this block
      let phi_block = ctx.new_block();

      // Jump from true branch to phi block
      ctx.enter_block(true_block);
      ctx.exit_block_br(phi_block);

      // Jump from false branch to phi block
      ctx.enter_block(false_block);
      ctx.exit_block_br(phi_block);

      // Create phi to choose value
      ctx.enter_block(phi_block);

      let l_phi = LLVMBuildPhi(
        ctx.l_builder,
        LLVMInt1TypeInContext(ctx.l_context),
        empty_cstr());

      LLVMAddIncoming(
        l_phi,
        [ ctx.build_bool(true), ctx.build_bool(false) ].as_mut_ptr() as _,
        [ true_block, false_block ].as_mut_ptr() as _,
        2);

      l_phi
    }
    RValue::Block { body, .. } => {
      let mut val = ctx.build_void();
      for expr in body.iter() {
        val = lower_rvalue(expr, ctx);
      }
      val
    }
    RValue::As { lhs, rhs, .. } => {
      let dest = lower_lvalue(lhs, ctx);
      let src = lower_rvalue(rhs, ctx);
      ctx.build_store(lhs.ty(), dest, src);
      // Void value
      ctx.build_void()
    }
    RValue::Rmw { op, lhs, rhs, .. } => {
      // LHS: We need both the address and value
      let dest_addr = lower_lvalue(lhs, ctx);
      let lhs_val = ctx.build_load(lhs.ty(), dest_addr);
      // RHS: We need only the value
      let rhs_val = lower_rvalue(rhs, ctx);
      // Then we can perform the computation and do the store
      let tmp_val = ctx.build_bin(lhs.ty(), *op, lhs_val, rhs_val);
      ctx.build_store(lhs.ty(), dest_addr, tmp_val);
      // Void value
      ctx.build_void()
    }
    RValue::Continue { .. } => {
      // Jump to continue point
      ctx.exit_block_br(*ctx.continue_to.last().unwrap());
      // Throw away code until next useful location
      let dead_block = ctx.new_block();
      ctx.enter_block(dead_block);
      // Void value
      ctx.build_void()
    }
    RValue::Break { .. } => {
      // Jump to break point
      ctx.exit_block_br(*ctx.break_to.last().unwrap());
      // Throw away code until next useful location
      let dead_block = ctx.new_block();
      ctx.enter_block(dead_block);
      // Void value
      ctx.build_void()
    }
    RValue::Return { arg, .. } => {
      let l_retval = lower_rvalue(&*arg, ctx);
      ctx.exit_block_ret(arg.ty(), l_retval);
      // Throw away code until next useful location
      let dead_block = ctx.new_block();
      ctx.enter_block(dead_block);
      // Void value
      ctx.build_void()
    }
    RValue::Let { index, init, .. } => {
      // Allocate variable
      let l_local = ctx.locals[*index];

      // Generate initializer
      if let Some(init) = init {
        let l_init = lower_rvalue(init, ctx);
        ctx.build_store(init.ty(), l_local, l_init);
      }

      // Void value
      ctx.build_void()
    }
    RValue::If { ty, cond, tbody, ebody, .. } => {
      let mut then_block = ctx.new_block();
      let mut else_block = ctx.new_block();
      let end_block = ctx.new_block();

      lower_bool(cond, ctx, then_block, else_block);

      ctx.enter_block(then_block);
      let l_then = lower_rvalue(tbody, ctx);
      // NOTE: we need to save the final blocks for the phi
      then_block = LLVMGetInsertBlock(ctx.l_builder);
      ctx.exit_block_br(end_block);

      ctx.enter_block(else_block);
      let l_else = lower_rvalue(ebody, ctx);
      else_block = LLVMGetInsertBlock(ctx.l_builder);
      ctx.exit_block_br(end_block);

      // End of if statement
      ctx.enter_block(end_block);

      // Create phi node
      if l_then.is_null() || l_else.is_null() {
        ctx.build_void()
      } else {
        let l_phi = LLVMBuildPhi(
          ctx.l_builder,
          ctx.lower_ty(ty),
          empty_cstr());

        LLVMAddIncoming(
          l_phi,
          [ l_then, l_else ].as_mut_ptr() as _,
          [ then_block, else_block ].as_mut_ptr() as _,
          2);

        l_phi
      }
    }
    RValue::While { cond, body, .. } => {
      let test_block = ctx.new_block();
      let body_block = ctx.new_block();
      let end_block = ctx.new_block();

      ctx.exit_block_br(test_block);

      // Initial block is the test as a demorgan expr
      ctx.enter_block(test_block);
      lower_bool(cond, ctx, body_block, end_block);

      // Next block is the loop body
      ctx.enter_block(body_block);
      ctx.continue_to.push(test_block);
      ctx.break_to.push(end_block);
      lower_rvalue(body, ctx);
      ctx.continue_to.pop();
      ctx.break_to.pop();
      ctx.exit_block_br(test_block);

      // End of the loop
      ctx.enter_block(end_block);

      // Void value
      ctx.build_void()
    }
    RValue::Loop { body, .. } => {
      let body_block = ctx.new_block();
      let end_block = ctx.new_block();

      ctx.exit_block_br(body_block);

      // Loop body in one block
      ctx.enter_block(body_block);
      ctx.continue_to.push(body_block);
      ctx.break_to.push(end_block);
      lower_rvalue(body, ctx);
      ctx.continue_to.pop();
      ctx.break_to.pop();
      ctx.exit_block_br(body_block);

      // End of the loop
      ctx.enter_block(end_block);

      // Void value
      ctx.build_void()
    }
    RValue::Match { ty, cond, cases, .. } => {
      let end_block = ctx.new_block();

      // Lower tag
      let l_addr = lower_lvalue(cond, ctx);
      let l_tag = ctx.build_load(&Ty::Int32, l_addr);
      let l_switch = LLVMBuildSwitch(
        ctx.l_builder,
        l_tag,
        end_block,
        cases.len() as _);

      let start_block = LLVMGetInsertBlock(ctx.l_builder);

      // Lower cases
      let mut phi_vals = Vec::new();
      let mut phi_blocks = Vec::new();

      for (index, (binding, val)) in cases.iter().enumerate() {
        let case_block = ctx.new_block();

        // Add branch from switch
        LLVMAddCase(l_switch,
                    ctx.build_int(&Ty::Int32, index),
                    case_block);

        // Lower case
        ctx.enter_block(case_block);
        if let Some(binding) = binding {
          assert_eq!(*binding, ctx.bindings.len());
          let l_binding = ctx.build_gep(cond.ty(), l_addr, 1);
          ctx.bindings.push(l_binding);
        }
        let l_val = lower_rvalue(val, ctx);
        if !l_val.is_null() {
          phi_vals.push(l_val);
          phi_blocks.push(LLVMGetInsertBlock(ctx.l_builder));
        }
        ctx.exit_block_br(end_block);
      }

      // Merge values into a phi at the end
      ctx.enter_block(end_block);

      if phi_vals.len() > 0 {
        let ty = ctx.lower_ty(ty);
        let l_phi = LLVMBuildPhi(
          ctx.l_builder,
          ty,
          empty_cstr());

        phi_vals.push(LLVMGetUndef(ty));
        phi_blocks.push(start_block);
        LLVMAddIncoming(l_phi,
                        phi_vals.as_ptr() as _,
                        phi_blocks.as_ptr() as _,
                        phi_vals.len() as _);
        l_phi
      } else {
        ctx.build_void()
      }
    }
  }
}

unsafe fn lower_bool(rvalue: &RValue, ctx: &mut LowerCtx, next1: BB, next2: BB) {
  match rvalue {
    RValue::LNot { arg, .. } => {
      lower_bool(arg, ctx, next2, next1);
    }
    RValue::LAnd { lhs, rhs, .. } => {
      let mid_block = ctx.new_block();
      lower_bool(lhs, ctx, mid_block, next2);
      ctx.enter_block(mid_block);
      lower_bool(rhs, ctx, next1, next2);
    }
    RValue::LOr { lhs, rhs, .. } => {
      let mid_block = ctx.new_block();
      lower_bool(lhs, ctx, next1, mid_block);
      ctx.enter_block(mid_block);
      lower_bool(rhs, ctx, next1, next2);
    }
    _ => {
      let cond = lower_rvalue(rvalue, ctx);
      ctx.exit_block_cond_br(cond, next1, next2);
    }
  }
}

struct LowerCtx<'a> {
  tctx: &'a mut TVarCtx,
  insts: &'a HashMap<(DefId, Vec<Ty>), Inst>,

  // Target machine
  l_machine: LLVMTargetMachineRef,
  l_layout: LLVMTargetDataRef,

  // LLVM handles
  l_context: LLVMContextRef,
  l_builder: LLVMBuilderRef,
  l_module: LLVMModuleRef,
  l_func: LLVMValueRef,
  l_alloca_block: LLVMBasicBlockRef,

  // Types
  types: HashMap<(DefId, Vec<Ty>), LLVMTypeRef>,

  // Values
  values: HashMap<(DefId, Vec<Ty>), LLVMValueRef>,

  // Anonymous structures
  // This de-duplication table is needed as LLVM doesn't
  // support checking for structural equality on such types
  // only nominal (e.g. always false between two different instances)
  anon_structs: HashMap<Vec<LLVMTypeRef>, LLVMTypeRef>,

  // String literals
  string_lits: HashMap<Vec<u8>, LLVMValueRef>,

  // Function parameters and locals
  params: Vec<LLVMValueRef>,
  locals: Vec<LLVMValueRef>,
  bindings: Vec<LLVMValueRef>,

  // Break and continue blocks
  break_to: Vec<LLVMBasicBlockRef>,
  continue_to: Vec<LLVMBasicBlockRef>
}

impl<'a> LowerCtx<'a> {
  unsafe fn new(tctx: &'a mut TVarCtx, insts: &'a HashMap<(DefId, Vec<Ty>), Inst>, module_id: RefStr) -> Self {
    LLVM_InitializeAllTargetInfos();
    LLVM_InitializeAllTargets();
    LLVM_InitializeAllTargetMCs();
    LLVM_InitializeAllAsmParsers();
    LLVM_InitializeAllAsmPrinters();

    let l_triple = LLVMGetDefaultTargetTriple();
    let l_cpu_name = LLVMGetHostCPUName();
    let l_cpu_features = LLVMGetHostCPUFeatures();

    let mut l_target = std::ptr::null_mut();
    let mut l_errors = std::ptr::null_mut();
    LLVMGetTargetFromTriple(l_triple, &mut l_target, &mut l_errors);
    assert!(l_errors.is_null());

    let l_machine = LLVMCreateTargetMachine(
      l_target,
      l_triple,
      l_cpu_name,
      l_cpu_features,
      LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault,
      LLVMRelocMode::LLVMRelocPIC,
      LLVMCodeModel::LLVMCodeModelDefault);

    let l_layout = LLVMCreateTargetDataLayout(l_machine);

    let l_context = LLVMContextCreate();
    let l_builder = LLVMCreateBuilderInContext(l_context);
    let l_module = LLVMModuleCreateWithNameInContext(module_id.borrow_c(), l_context);

    LLVMSetTarget(l_module, l_triple);
    let l_layout_str = LLVMCopyStringRepOfTargetData(l_layout);
    LLVMSetDataLayout(l_module, l_layout_str);
    LLVMDisposeMessage(l_layout_str);

    LLVMDisposeMessage(l_triple);
    LLVMDisposeMessage(l_cpu_name);
    LLVMDisposeMessage(l_cpu_features);

    LowerCtx {
      tctx,
      insts,

      l_machine,
      l_layout,
      l_context,
      l_builder,
      l_module,
      l_func: std::ptr::null_mut(),

      l_alloca_block: std::ptr::null_mut(),

      types: HashMap::new(),
      values: HashMap::new(),

      anon_structs: HashMap::new(),
      string_lits: HashMap::new(),

      params: Vec::new(),
      locals: Vec::new(),
      bindings: Vec::new(),

      break_to: Vec::new(),
      continue_to: Vec::new()
    }
  }

  unsafe fn get_type(&mut self, id: &(DefId, Vec<Ty>)) -> LLVMTypeRef {
    let id = (id.0, self.tctx.root_type_args(&id.1));

    if let Some(ty) = self.types.get(&id) {
      *ty
    } else {
      let inst = self.insts.get(&id).unwrap();
      let ty = self.lower_ty_def(inst);
      self.types.insert(id, ty);
      ty
    }
  }

  unsafe fn lower_ty_def(&mut self, inst: &Inst) -> LLVMTypeRef {
    let (name, l_params) = match inst {
      Inst::Struct { name, params: Some(params), .. } => {
        // This is the simplest case, LLVM has native support for structures
        (*name, params
          .iter()
          .map(|(_, ty)| self.lower_ty(ty))
          .collect())
      }
      Inst::Union { name, params: Some(params), .. } => {
        // The union lowering code is shared with enums thus it's in 'lower_union'
        let l_params = params
          .iter()
          .map(|(_, ty)| self.lower_ty(ty))
          .collect();

        (*name, self.lower_union(l_params))
      }
      Inst::Enum { name, variants: Some(variants), .. } => {
        // Enum lowering is done by adding a discriminant (always a dword for now)
        // Followed by the variants lowered as if they were parameters of a union

        // Convert struct-like variants into LLVM types
        let mut l_variant_types = vec![];
        for variant in variants {
          match variant {
            Variant::Unit(_) => (),
            Variant::Struct(_, params) => {
              let l_params: Vec<LLVMTypeRef> = params
                .iter()
                .map(|(_, ty)| self.lower_ty(ty))
                .collect();
              l_variant_types.push(self.lower_anon_struct(&l_params));
            }
          }
        }

        // Create actual enum parameters
        (*name, concat(
          vec![ LLVMInt32TypeInContext(self.l_context) ],
          self.lower_union(l_variant_types)
        ))
      }
      _ => unreachable!(),
    };

    let l_type = LLVMStructCreateNamed(self.l_context, name.borrow_c());
    LLVMStructSetBody(l_type,
                      l_params.as_ptr() as _,
                      l_params.len() as _,
                      0);
    l_type
  }

  unsafe fn lower_union(&mut self, l_params: Vec<LLVMTypeRef>) -> Vec<LLVMTypeRef> {
    // NOTE: this special case is needed otherwise bad things (NULL-derefs happen)
    if l_params.len() == 0 {
      return vec![]
    }

    // Union lowering is done clang style, we take the highest alignment
    // element, and pad it to have the expected size of the union
    let mut union_align = 0;
    let mut union_size = 0;
    let mut l_max_align_type = std::ptr::null_mut();
    for l_param in l_params {
      assert!(LLVMTypeIsSized(l_param) == 1);
      if self.align_of(l_param) > union_align {
        union_align = self.align_of(l_param);
        l_max_align_type = l_param;
      }
      if self.size_of(l_param) > union_size {
        union_size = self.size_of(l_param);
      }
    }

    // Start with the highest alignment type then add byte array with
    // the length of the required padding
    let mut l_params = vec![ l_max_align_type ];
    let padding_size = union_size - self.size_of(l_max_align_type);
    if padding_size > 0 {
      l_params.push(LLVMArrayType(
        LLVMInt8TypeInContext(self.l_context), padding_size as u32));
    }
    l_params
  }

  unsafe fn align_of(&mut self, l_type: LLVMTypeRef) -> usize {
    LLVMPreferredAlignmentOfType(self.l_layout, l_type) as usize
  }

  unsafe fn size_of(&mut self, l_type: LLVMTypeRef) -> usize {
    LLVMStoreSizeOfType(self.l_layout, l_type) as usize
  }

  unsafe fn lower_ty(&mut self, ty: &Ty) -> LLVMTypeRef {
    use Ty::*;

    // Void semantic types are special
    match self.ty_semantics(ty) {
      Semantics::Void => return LLVMVoidTypeInContext(self.l_context),
      Semantics::Addr | Semantics::Value => (),
    }

    match &self.tctx.lit_ty(ty) {
      Bool => LLVMInt1TypeInContext(self.l_context),
      Uint8 | Int8 => LLVMInt8TypeInContext(self.l_context),
      Uint16 | Int16 => LLVMInt16TypeInContext(self.l_context),
      Uint32 | Int32 => LLVMInt32TypeInContext(self.l_context),
      Uint64 | Int64 => LLVMInt64TypeInContext(self.l_context),
      // FIXME: make the width of Uintn and Intn per target
      Uintn | Intn => LLVMInt64TypeInContext(self.l_context),
      Float => LLVMFloatTypeInContext(self.l_context),
      Double => LLVMDoubleTypeInContext(self.l_context),
      StructRef(_, id) |
      UnionRef(_, id) |
      EnumRef(_, id) => {
        self.get_type(id)
      }
      Ptr(..) |
      Func(..) => {
        LLVMPointerTypeInContext(self.l_context, 0)
      }
      Arr(siz, elem_ty) => {
        LLVMArrayType(self.lower_ty(elem_ty), *siz as u32)
      }
      Tuple(params) => {
        let l_params: Vec<LLVMTypeRef> = params
          .iter()
          .map(|(_, ty)| self.lower_ty(ty))
          .collect();
        self.lower_anon_struct(&l_params)
      }
      _ => unreachable!()
    }
  }

  unsafe fn lower_anon_struct(&mut self, fields: &[LLVMTypeRef]) -> LLVMTypeRef {
    let l_context = self.l_context;
    *self.anon_structs
      .raw_entry_mut()
      .from_key(fields)
      .or_insert_with(|| {
        (Vec::from(fields), LLVMStructTypeInContext(l_context,
                                                    fields.as_ptr() as _,
                                                    fields.len() as _,
                                                    0))
      }).1
  }

  unsafe fn lower_func_ty(&mut self, params: &Vec<(RefStr, Ty)>, va: bool, ret_ty: &Ty) -> LLVMTypeRef {
    let mut l_params: Vec<LLVMTypeRef> = params
      .iter()
      .map(|(_, ty)| {
        match self.ty_semantics(ty) {
          Semantics::Void => todo!(),
          Semantics::Value => self.lower_ty(ty),
          Semantics::Addr => LLVMPointerTypeInContext(self.l_context, 0),
        }
      })
      .collect();

    match self.ty_semantics(ret_ty) {
      Semantics::Void | Semantics::Value => {
        LLVMFunctionType(self.lower_ty(ret_ty),
                         l_params.as_mut_ptr() as _,
                         l_params.len() as _,
                         va as _)
      }
      Semantics::Addr => {
        let mut real_params = vec![ LLVMPointerTypeInContext(self.l_context, 0) ];
        real_params.extend(l_params);

        LLVMFunctionType(LLVMVoidTypeInContext(self.l_context),
                         real_params.as_mut_ptr() as _,
                         real_params.len() as _,
                         va as _)
      }
    }
  }

  fn get_value(&mut self, id: &(DefId, Vec<Ty>)) -> LLVMValueRef {
    let tmp = (id.0, self.tctx.root_type_args(&id.1));
    *self.values.get(&tmp).unwrap()
  }


  unsafe fn build_void(&mut self) -> LLVMValueRef {
    std::ptr::null_mut()
  }

  unsafe fn build_string_lit(&mut self, data: &[u8]) -> LLVMValueRef {
    // Borrow checker :/
    let l_module = self.l_module;
    let l_context = self.l_context;
    let index = self.string_lits.len();

    *self.string_lits.raw_entry_mut().from_key(data).or_insert_with(|| {
      // Create name
      let name = RefStr::new(&format!(".str.{}", index));

      // Create global
      let len = data.len() as u32;
      let val = LLVMAddGlobal(l_module,
                              LLVMArrayType(
                                LLVMInt8TypeInContext(l_context),
                                // NOTE: +1 for NUL terminator
                                len + 1),
                              name.borrow_c());

      // Set initializer
      // NOTE: for now these are NUL-terminated
      LLVMSetInitializer(val, LLVMConstStringInContext(
                           l_context,
                           data.as_ptr() as _,
                           len,
                           0));

      (data.to_vec(), val)
    }).1
  }

  unsafe fn build_bool(&mut self, val: bool) -> LLVMValueRef {
    LLVMConstInt(LLVMInt1TypeInContext(self.l_context), val as u64, 0)
  }

  unsafe fn build_int(&mut self, ty: &Ty, val: usize) -> LLVMValueRef {
    LLVMConstInt(self.lower_ty(ty), val as u64, 0)
  }

  unsafe fn build_flt(&mut self, ty: &Ty, val: f64) -> LLVMValueRef {
    LLVMConstReal(self.lower_ty(ty), val)
  }

  unsafe fn build_const_gep(&mut self, ty: &Ty, l_ptr: LLVMValueRef, idx: usize) -> LLVMValueRef {
    let mut indices = [
      LLVMConstInt(LLVMInt8TypeInContext(self.l_context), 0, 0),
      // NOTE: this is not documented in many places, but struct field
      // indices have to be Int32 otherwise LLVM crashes :(
      LLVMConstInt(LLVMInt32TypeInContext(self.l_context), idx as u64, 0)
    ];

    let l_type = self.lower_ty(ty);
    LLVMConstInBoundsGEP2(l_type,
                          l_ptr,
                          &mut indices as *mut LLVMValueRef,
                          indices.len() as u32)

  }

  unsafe fn allocate_local(&mut self, ty: &Ty) -> LLVMValueRef {
    match self.ty_semantics(ty) {
      Semantics::Void => std::ptr::null_mut(),
      Semantics::Addr | Semantics::Value => {
        let prev = LLVMGetInsertBlock(self.l_builder);
        self.enter_block(self.l_alloca_block);
        let l_alloca= LLVMBuildAlloca(
          self.l_builder,
          self.lower_ty(ty),
          empty_cstr());
        self.enter_block(prev);
        l_alloca
      }
    }
  }

  unsafe fn new_block(&mut self) -> LLVMBasicBlockRef {
    assert!(!self.l_func.is_null());
    LLVMAppendBasicBlock(self.l_func, empty_cstr())
  }

  unsafe fn enter_block(&mut self, block: LLVMBasicBlockRef) {
    LLVMPositionBuilderAtEnd(self.l_builder, block);
  }

  unsafe fn exit_block_br(&mut self, dest: LLVMBasicBlockRef) {
    LLVMBuildBr(self.l_builder, dest);
  }

  unsafe fn exit_block_cond_br(&mut self, cond: LLVMValueRef,
                               dest1: LLVMBasicBlockRef,
                               dest2: LLVMBasicBlockRef) {
    LLVMBuildCondBr(self.l_builder, cond, dest1, dest2);
  }

  unsafe fn exit_block_ret(&mut self, ty: &Ty, val: LLVMValueRef) {
    match self.ty_semantics(ty) {
      Semantics::Void => {
        LLVMBuildRetVoid(self.l_builder);
      }
      Semantics::Value => {
        LLVMBuildRet(self.l_builder, val);
      }
      Semantics::Addr => {
        self.build_store(ty, LLVMGetParam(self.l_func, 0), val);
        LLVMBuildRetVoid(self.l_builder);
      }
    }
  }

  unsafe fn build_load(&mut self, ty: &Ty, l_ptr: LLVMValueRef) -> LLVMValueRef {
    let l_type = self.lower_ty(ty);
    match self.ty_semantics(ty) {
      Semantics::Void => std::ptr::null_mut(),
      Semantics::Addr => l_ptr,
      Semantics::Value => LLVMBuildLoad2(self.l_builder,
                                         l_type,
                                         l_ptr,
                                         empty_cstr())
    }
  }

  unsafe fn build_store(&mut self, ty: &Ty, l_dest: LLVMValueRef, l_src: LLVMValueRef) {
    match self.ty_semantics(ty) {
      Semantics::Void => {}
      Semantics::Addr => {
        let l_type = self.lower_ty(ty);
        let align = self.align_of(l_type) as u32;
        let size = LLVMConstInt(LLVMInt32TypeInContext(self.l_context),
                                self.size_of(l_type) as u64, 0);
        LLVMBuildMemCpy(self.l_builder, l_dest, align, l_src, align, size);
      }
      Semantics::Value => {
        LLVMBuildStore(self.l_builder, l_src, l_dest);
      }
    }
  }

  unsafe fn ty_semantics(&mut self, ty: &Ty) -> Semantics {
    use Ty::*;

    // Get literal type
    let ty = self.tctx.lit_ty(ty);

    // Choose semantics
    match self.tctx.lit_ty(&ty) {
      Unit => Semantics::Void,
      Bool | Uint8 | Int8 | Uint16 |
      Int16 |Uint32 | Int32 | Uint64 |
      Int64 | Uintn | Intn | Float |
      Double | Ptr(..) | Func(..) => Semantics::Value,
      Arr(..) |
      Tuple(..) |
      StructRef(..) |
      UnionRef(..) |
      EnumRef(..) => Semantics::Addr,
      _ => unreachable!()
    }
  }

  unsafe fn build_aggregate_inplace(&mut self, ty: &Ty, l_storage: LLVMValueRef, fields: &[(Ty, LLVMValueRef)]) {
    for (idx, (field_ty, l_field)) in fields.iter().enumerate() {
      let l_dest = self.build_gep(ty, l_storage, idx);
      self.build_store(field_ty, l_dest, *l_field);
    }
  }

  unsafe fn build_gep(&mut self, ty: &Ty, l_ptr: LLVMValueRef, idx: usize) -> LLVMValueRef {
    let mut indices = [
      LLVMConstInt(LLVMInt8TypeInContext(self.l_context), 0, 0),
      // NOTE: this is not documented in many places, but struct field
      // indices have to be Int32 otherwise LLVM crashes :(
      LLVMConstInt(LLVMInt32TypeInContext(self.l_context), idx as u64, 0)
    ];

    let l_type = self.lower_ty(ty);
    LLVMBuildInBoundsGEP2(self.l_builder,
                         l_type,
                         l_ptr,
                         &mut indices as *mut LLVMValueRef,
                         indices.len() as u32,
                         empty_cstr())
  }

  unsafe fn build_index(&mut self, ty: &Ty, l_ptr: LLVMValueRef, l_idx: LLVMValueRef) -> LLVMValueRef {
    let mut indices = [
      LLVMConstInt(LLVMInt8TypeInContext(self.l_context), 0, 0),
      l_idx
    ];

    let l_type = self.lower_ty(ty);
    LLVMBuildInBoundsGEP2(self.l_builder,
                          l_type,
                          l_ptr,
                          indices.as_mut_ptr() as _,
                          indices.len() as _,
                          empty_cstr())
  }

  unsafe fn build_call(&mut self, func_ty: &Ty, l_func: LLVMValueRef, mut l_args: Vec<LLVMValueRef>) -> LLVMValueRef {
    let (params, va, ret_ty) = if let Ty::Func(params, va, ret_ty) = func_ty {
      (params, va, ret_ty)
    } else {
      unreachable!()
    };

    let l_func_type = self.lower_func_ty(params, *va, ret_ty);

    match self.ty_semantics(ret_ty) {
      Semantics::Addr => {
        let l_ret_tmp = self.allocate_local(ret_ty);
        let mut real_args = vec![l_ret_tmp];
        real_args.extend(l_args);
        LLVMBuildCall2(self.l_builder,
                       l_func_type,
                       l_func,
                       real_args.as_mut_ptr() as _,
                       real_args.len() as _,
                       empty_cstr());
        l_ret_tmp
      }
      _ => {
        LLVMBuildCall2(self.l_builder,
                       l_func_type,
                       l_func,
                       l_args.as_mut_ptr() as _,
                       l_args.len() as _,
                       empty_cstr())
      }
    }
  }

  unsafe fn build_un(&mut self, ty: &Ty, op: UnOp, l_arg: LLVMValueRef) -> LLVMValueRef {
    use Ty::*;
    use UnOp::*;

    match (op, self.tctx.lit_ty(ty)) {
      (UPlus, Uint8 | Int8 | Uint16 | Int16 | Uint32 | Int32 | Uint64 | Int64 | Uintn | Intn | Float | Double) => {
        l_arg
      }
      (UMinus, Uint8 | Int8 | Uint16 | Int16 | Uint32 | Int32 | Uint64 | Int64 | Uintn | Intn) => {
        LLVMBuildNeg(self.l_builder, l_arg, empty_cstr())
      }
      (UMinus, Float | Double) => {
        LLVMBuildFNeg(self.l_builder, l_arg, empty_cstr())
      }
      (Not, Uint8 | Int8 | Uint16 | Int16 | Uint32 | Int32 | Uint64 | Int64 | Uintn | Intn) => {
        LLVMBuildNot(self.l_builder, l_arg, empty_cstr())
      }
      _ => unreachable!()
    }
  }

  unsafe fn build_cast(&mut self, dest_ty: &Ty, src_ty: &Ty, l_val: LLVMValueRef) -> LLVMValueRef {
    use Ty::*;

    let dest_ty = self.tctx.lit_ty(dest_ty);
    let src_ty = self.tctx.lit_ty(src_ty);

    if dest_ty == src_ty { // Nothing to cast
      return l_val
    }

    let l_dest_type = self.lower_ty(&dest_ty);
    let l_src_type = self.lower_ty(&src_ty);

    match (&dest_ty, &src_ty) {
      // Pointer to pointer
      (Ptr(..), Ptr(..)) => {
        l_val
      }
      // Pointer to integer
      (Uint8|Uint16|Uint32|Uint64|Uintn|Int8|Int16|Int32|Int64|Intn, Ptr(..)) => {
        LLVMBuildPtrToInt(self.l_builder, l_val, l_dest_type, empty_cstr())
      }
      // Integer to pointer
      (Ptr(..), Uint8|Uint16|Uint32|Uint64|Uintn|Int8|Int16|Int32|Int64|Intn) => {
        LLVMBuildIntToPtr(self.l_builder, l_val, l_dest_type, empty_cstr())
      }
      // Truncate double to float
      (Float, Double) => {
        LLVMBuildFPTrunc(self.l_builder, l_val, l_dest_type, empty_cstr())
      }
      // Extend float to double
      (Double, Float) => {
        LLVMBuildFPExt(self.l_builder, l_val, l_dest_type, empty_cstr())
      }
      // unsigned integer to floating point
      (Float|Double, Uint8|Uint16|Uint32|Uint64|Uintn) => {
        LLVMBuildUIToFP(self.l_builder, l_val, l_dest_type, empty_cstr())
      }
      // signed integer to floating point
      (Float|Double, Int8|Int16|Int32|Int64|Intn) => {
        LLVMBuildSIToFP(self.l_builder, l_val, l_dest_type, empty_cstr())
      }
      // floating point to unsigned integer
      (Uint8|Uint16|Uint32|Uint64|Uintn, Float|Double) => {
        LLVMBuildFPToUI(self.l_builder, l_val, l_dest_type, empty_cstr())
      }
      // floating point to signed integer
      (Int8|Int16|Int32|Int64|Intn, Float|Double) => {
        LLVMBuildFPToSI(self.l_builder, l_val, l_dest_type, empty_cstr())
      }
      // integer to integer conversions
      (Uint8|Uint16|Uint32|Uint64|Uintn|Int8|Int16|Int32|Int64|Intn,
          Uint8|Uint16|Uint32|Uint64|Uintn|Int8|Int16|Int32|Int64|Intn) => {
        let dest_size = self.size_of(l_dest_type);
        let src_size = self.size_of(l_src_type);
        if dest_size == src_size {  // LLVM disregards signedness, so nothing to do
          return l_val
        } else if dest_size < src_size {
          LLVMBuildTrunc(self.l_builder, l_val, l_dest_type, empty_cstr())
        } else {
          // Choose sign or zero extension based on destination type
          match &dest_ty {
            Int8|Int16|Int32|Int64|Intn => LLVMBuildSExt(self.l_builder, l_val, l_dest_type, empty_cstr()),
            _ => LLVMBuildZExt(self.l_builder, l_val, l_dest_type, empty_cstr())
          }
        }
      }
      _ => unreachable!()
    }
  }

  unsafe fn build_bin(&mut self, ty: &Ty, op: BinOp, l_lhs: LLVMValueRef, l_rhs: LLVMValueRef) -> LLVMValueRef {
    use Ty::*;
    use BinOp::*;

    match (op, self.tctx.lit_ty(ty)) {
      // Integer multiply
      (Mul, Uint8 | Int8 | Uint16 | Int16 | Uint32 | Int32 | Uint64 | Int64 | Uintn | Intn) => {
        LLVMBuildMul(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Floating point multiply
      (Mul, Float | Double) => {
        LLVMBuildFMul(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Unsigned integer divide
      (Div, Uint8 | Uint16 | Uint32 | Uint64 | Uintn) => {
        LLVMBuildUDiv(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Signed integer divide
      (Div, Int8 | Int16 | Int32 | Int64 | Intn) => {
        LLVMBuildSDiv(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Floating point divide
      (Div, Float | Double) => {
        LLVMBuildFDiv(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Unsigned integer modulo
      (Mod, Uint8 | Uint16 | Uint32 | Uint64 | Uintn) => {
        LLVMBuildURem(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Signed integer modulo
      (Mod, Int8 | Int16 | Int32 | Int64 | Intn) => {
        LLVMBuildSRem(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Integer addition
      (Add, Uint8 | Int8 | Uint16 | Int16 | Uint32 | Int32 | Uint64 | Int64 | Uintn | Intn) => {
        LLVMBuildAdd(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Floating point addition
      (Add, Float | Double) => {
        LLVMBuildFAdd(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Integer substraction
      (Sub, Uint8 | Int8 | Uint16 | Int16 | Uint32 | Int32 | Uint64 | Int64 | Uintn | Intn) => {
        LLVMBuildSub(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Floating point substraction
      (Sub, Float | Double) => {
        LLVMBuildFSub(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Left shift
      (Lsh, Uint8 | Int8 | Uint16 | Int16 | Uint32 | Int32 | Uint64 | Int64 | Uintn | Intn) => {
        LLVMBuildShl(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Unsigned (logical) right shift
      (Rsh, Uint8 | Uint16 | Uint32 | Uint64 | Uintn) => {
        LLVMBuildLShr(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Signed (arithmetic) right shift
      (Rsh, Int8 | Int16 | Int32 | Int64 | Intn) => {
        LLVMBuildAShr(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Bitwise and
      (And, Uint8 | Int8 | Uint16 | Int16 | Uint32 | Int32 | Uint64 | Int64 | Uintn | Intn) => {
        LLVMBuildAnd(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Bitwise xor
      (Xor, Uint8 | Int8 | Uint16 | Int16 | Uint32 | Int32 | Uint64 | Int64 | Uintn | Intn) => {
        LLVMBuildXor(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Bitwise or
      (Or, Uint8 | Int8 | Uint16 | Int16 | Uint32 | Int32 | Uint64 | Int64 | Uintn | Intn) => {
        LLVMBuildOr(self.l_builder, l_lhs, l_rhs, empty_cstr())
      }
      // Integer equality and inequality
      (Eq, Uint8 | Int8 | Uint16 | Int16 | Uint32 | Int32 | Uint64 | Int64 | Uintn | Intn) => {
        LLVMBuildICmp(self.l_builder, LLVMIntEQ, l_lhs, l_rhs, empty_cstr())
      }
      (Ne, Uint8 | Int8 | Uint16 | Int16 | Uint32 | Int32 | Uint64 | Int64 | Uintn | Intn) => {
        LLVMBuildICmp(self.l_builder, LLVMIntNE, l_lhs, l_rhs, empty_cstr())
      }
      // Unsigned integer comparisons
      (Lt, Uint8 | Uint16 | Uint32 | Uint64 | Uintn) => {
        LLVMBuildICmp(self.l_builder, LLVMIntULT, l_lhs, l_rhs, empty_cstr())
      }
      (Gt, Uint8 | Uint16 | Uint32 | Uint64 | Uintn) => {
        LLVMBuildICmp(self.l_builder, LLVMIntUGT, l_lhs, l_rhs, empty_cstr())
      }
      (Le, Uint8 | Uint16 | Uint32 | Uint64 | Uintn) => {
        LLVMBuildICmp(self.l_builder, LLVMIntULE, l_lhs, l_rhs, empty_cstr())
      }
      (Ge, Uint8 | Uint16 | Uint32 | Uint64 | Uintn) => {
        LLVMBuildICmp(self.l_builder, LLVMIntUGE, l_lhs, l_rhs, empty_cstr())
      }
      // Signed integer comparisons
      (Lt, Int8 | Int16 | Int32 | Int64 | Intn) => {
        LLVMBuildICmp(self.l_builder, LLVMIntSLT, l_lhs, l_rhs, empty_cstr())
      }
      (Gt, Int8 | Int16 | Int32 | Int64 | Intn) => {
        LLVMBuildICmp(self.l_builder, LLVMIntSGT, l_lhs, l_rhs, empty_cstr())
      }
      (Le, Int8 | Int16 | Int32 | Int64 | Intn) => {
        LLVMBuildICmp(self.l_builder, LLVMIntSLE, l_lhs, l_rhs, empty_cstr())
      }
      (Ge, Int8 | Int16 | Int32 | Int64 | Intn) => {
        LLVMBuildICmp(self.l_builder, LLVMIntSGE, l_lhs, l_rhs, empty_cstr())
      }
      // Float Comparisons
      (Eq, Float | Double) => {
        LLVMBuildFCmp(self.l_builder, LLVMRealOEQ, l_lhs, l_rhs, empty_cstr())
      }
      (Ne, Float | Double) => {
        LLVMBuildFCmp(self.l_builder, LLVMRealONE, l_lhs, l_rhs, empty_cstr())
      }
      (Lt, Float | Double) => {
        LLVMBuildFCmp(self.l_builder, LLVMRealOLT, l_lhs, l_rhs, empty_cstr())
      }
      (Gt, Float | Double) => {
        LLVMBuildFCmp(self.l_builder, LLVMRealOGT, l_lhs, l_rhs, empty_cstr())
      }
      (Le, Float | Double) => {
        LLVMBuildFCmp(self.l_builder, LLVMRealOLE, l_lhs, l_rhs, empty_cstr())
      }
      (Ge, Float | Double) => {
        LLVMBuildFCmp(self.l_builder, LLVMRealOGE, l_lhs, l_rhs, empty_cstr())
      }
      _ => unreachable!()
    }
  }

  unsafe fn lower_defs(&mut self) {
    // Pass 1: Create LLVM values for each definition
    for (id, def) in self.insts.iter() {
      let l_value = match def {
        Inst::Data { name, init, .. } => {
          let l_type = const_init_ty(init, self);
          LLVMAddGlobal(self.l_module, l_type, name.borrow_c())
        }
        Inst::ExternData { name, ty, .. } => {
          LLVMAddGlobal(self.l_module, self.lower_ty(ty), name.borrow_c())
        }
        Inst::Func { name, ty, .. } |
        Inst::ExternFunc { name, ty, .. } => {
          if let Ty::Func(params, va, ret_ty) = ty {
            LLVMAddFunction(self.l_module,
                            name.borrow_c(),
                            self.lower_func_ty(params, *va, ret_ty))
          } else {
            unreachable!()
          }
        }
        _ => continue
      };

      self.values.insert(id.clone(), l_value);
    }
    // Pass 2: Lower initializers and function bodies
    for (id, def) in self.insts.iter() {
      match def {
        Inst::Data { init, .. }  => {
          let l_value = self.get_value(id);
          let l_init = lower_const_val(init, self);
          assert_eq!(LLVMGlobalGetValueType(l_value), LLVMTypeOf(l_init));
          LLVMSetInitializer(l_value, l_init);
        }
        Inst::Func { params, locals, body: Some(body), .. } => {
          self.l_func = self.get_value(id);

          // Create prelude block for allocas
          self.l_alloca_block = self.new_block();
          self.enter_block(self.l_alloca_block);

          // Calculate parameter base index
          let pbase = if let Semantics::Addr = self.ty_semantics(body.ty()) { 1 } else { 0 };

          // Allocate parameters
          self.params.clear();
          for (index, (_, ty)) in params.iter().enumerate() {
            let l_alloca = self.allocate_local(ty);
            let l_param = LLVMGetParam(self.l_func, pbase + index as u32);
            self.build_store(ty, l_alloca, l_param);
            self.params.push(l_alloca);
          }
          // Allocate locals
          self.locals.clear();
          for (_, ty) in locals.iter() {
            let l_alloca = self.allocate_local(ty);
            self.locals.push(l_alloca);
          }

          // Create LLVM function body
          let body_block = self.new_block();
          self.enter_block(body_block);
          let l_retval = lower_rvalue(body, self);
          self.exit_block_ret(body.ty(), l_retval);

          // Add branch from allocas to body
          self.enter_block(self.l_alloca_block);
          self.exit_block_br(body_block);
        }
        _ => ()
      }
    }
  }

  unsafe fn dump(&self) {
    LLVMDumpModule(self.l_module)
  }

  unsafe fn write_llvm_ir(&self, path: &Path) -> MRes<()> {
    // Create string representation of module
    let module_str = LLVMPrintModuleToString(self.l_module);

    // Write string to file
    let data: &[u8] = std::slice::from_raw_parts(
      module_str as *const u8,
      c_strlen(module_str));
    std::fs::write(path, data)?;

    // Free string
    LLVMDisposeMessage(module_str);

    // We are okay
    Ok(())
  }

  unsafe fn write_machine_code(&self, textual: bool, path: &Path) -> MRes<()> {
    let file_type = if textual {
      LLVMCodeGenFileType::LLVMAssemblyFile
    } else {
      LLVMCodeGenFileType::LLVMObjectFile
    };

    let mut errors = std::ptr::null_mut();
    let mut buffer = std::ptr::null_mut();

    // Ask LLVM put the data into a buffer for us
    LLVMTargetMachineEmitToMemoryBuffer(
      self.l_machine,
      self.l_module,
      file_type,
      &mut errors,
      &mut buffer);

    // NOTE: Generating un-compilable IR is considered a bug
    assert!(errors.is_null());

    // Write the data from above to the output file
    let data: &[u8] = std::slice::from_raw_parts(
      LLVMGetBufferStart(buffer) as *const u8,
      LLVMGetBufferSize(buffer));
    std::fs::write(path, data)?;

    // Free buffer
    LLVMDisposeMemoryBuffer(buffer);

    // We are all okay
    Ok(())
  }
}

impl<'a> Drop for LowerCtx<'a> {
  fn drop(&mut self) {
    unsafe {
      LLVMDisposeTargetMachine(self.l_machine);
      LLVMDisposeTargetData(self.l_layout);
      LLVMDisposeBuilder(self.l_builder);
      LLVMDisposeModule(self.l_module);
      LLVMContextDispose(self.l_context);
    }
  }
}

pub(super) fn lower_module(tctx: &mut TVarCtx, insts: &HashMap<(DefId, Vec<Ty>), Inst>, path: &Path, compile_to: CompileTo) -> MRes<()> {
  unsafe {
    let mut ctx = LowerCtx::new(tctx, insts, RefStr::new(""));
    ctx.lower_defs();
    if let Some(_) = option_env!("MPC_SPEW") {
      ctx.dump();
    }
    match compile_to {
      CompileTo::LLVMIr => ctx.write_llvm_ir(path)?,
      CompileTo::Assembly => ctx.write_machine_code(true, path)?,
      CompileTo::Object => ctx.write_machine_code(false, path)?,
    };
    Ok(())
  }
}
