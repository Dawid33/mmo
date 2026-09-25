//! Throwaway: load-time instrumentation for deterministic mods.
//!   fw <in.wasm> <out.wasm> [nan] [gas]
use finite_wasm::{prefix_sum_vec, wasmparser as wp};
use wasm_encoder::reencode::{utils, Error, Reencode};
use wasm_encoder::{CodeSection, Function, FunctionSection, Instruction, TypeSection, ValType};

// ---------- NaN canonicalization pass ----------
struct NanCanon { ty32: u32, ty64: u32, f32_fn: u32, f64_fn: u32, sites: u32, inline: bool, param_counts: Vec<u32>, func_types: Vec<u32>, cur: usize }

impl Reencode for NanCanon {
    type Error = std::convert::Infallible;

    fn parse_type_section(&mut self, types: &mut TypeSection, section: wp::TypeSectionReader<'_>) -> Result<(), Error<Self::Error>> {
        utils::parse_type_section(self, types, section)?;
        assert_eq!(types.len(), self.ty32);
        types.ty().function([ValType::F32], [ValType::F32]);
        types.ty().function([ValType::F64], [ValType::F64]);
        Ok(())
    }
    fn parse_function_section(&mut self, funcs: &mut FunctionSection, section: wp::FunctionSectionReader<'_>) -> Result<(), Error<Self::Error>> {
        utils::parse_function_section(self, funcs, section)?;
        funcs.function(self.ty32);
        funcs.function(self.ty64);
        Ok(())
    }
    fn parse_code_section(&mut self, code: &mut CodeSection, section: wp::CodeSectionReader<'_>) -> Result<(), Error<Self::Error>> {
        utils::parse_code_section(self, code, section)?;
        // canon(x) = select(x, canonical_nan, x == x)
        let mut f = Function::new([]);
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::F32Const(f32::from_bits(0x7fc0_0000)));
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::F32Eq);
        f.instruction(&Instruction::Select);
        f.instruction(&Instruction::End);
        code.function(&f);
        let mut f = Function::new([]);
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::F64Const(f64::from_bits(0x7ff8_0000_0000_0000)));
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::F64Eq);
        f.instruction(&Instruction::Select);
        f.instruction(&Instruction::End);
        code.function(&f);
        Ok(())
    }
    fn parse_function_body(&mut self, code: &mut CodeSection, func: wp::FunctionBody<'_>) -> Result<(), Error<Self::Error>> {
        use wp::Operator::*;
        let mut locals = Vec::new();
        let mut n_locals = self.param_counts[self.func_types[self.cur] as usize];
        self.cur += 1;
        for pair in func.get_locals_reader()? { let (cnt, ty) = pair?; n_locals += cnt; locals.push((cnt, self.val_type(ty)?)); }
        let (l32, l64) = (n_locals, n_locals + 1);
        if self.inline { locals.push((1, ValType::F32)); locals.push((1, ValType::F64)); }
        let mut f = Function::new(locals);
        let mut reader = func.get_operators_reader()?;
        while !reader.eof() {
            let op = reader.read()?;
            let canon = match op {
                F32Add | F32Sub | F32Mul | F32Div | F32Sqrt | F32Min | F32Max | F32Ceil | F32Floor
                | F32Trunc | F32Nearest | F32DemoteF64 => Some(self.f32_fn),
                F64Add | F64Sub | F64Mul | F64Div | F64Sqrt | F64Min | F64Max | F64Ceil | F64Floor
                | F64Trunc | F64Nearest | F64PromoteF32 => Some(self.f64_fn),
                _ => None,
            };
            f.instruction(&self.instruction(op)?);
            if let Some(h) = canon {
                self.sites += 1;
                if !self.inline { f.instruction(&Instruction::Call(h)); }
                else if h == self.f32_fn {
                    for i in [Instruction::LocalTee(l32), Instruction::F32Const(f32::from_bits(0x7fc0_0000)), Instruction::LocalGet(l32), Instruction::LocalGet(l32), Instruction::F32Eq, Instruction::Select] { f.instruction(&i); }
                } else {
                    for i in [Instruction::LocalTee(l64), Instruction::F64Const(f64::from_bits(0x7ff8_0000_0000_0000)), Instruction::LocalGet(l64), Instruction::LocalGet(l64), Instruction::F64Eq, Instruction::Select] { f.instruction(&i); }
                }
            }
        }
        code.function(&f);
        Ok(())
    }
}

fn nan_canon(wasm: &[u8], inline: bool) -> Vec<u8> {
    let (mut types, mut funcs) = (0u32, 0u32);
    let (mut param_counts, mut func_types) = (Vec::new(), Vec::new());
    for p in wp::Parser::new(0).parse_all(wasm) {
        match p.unwrap() {
            wp::Payload::TypeSection(s) => { types = s.count(); for t in s.into_iter_err_on_gc_types() { param_counts.push(t.unwrap().params().len() as u32); } }
            wp::Payload::ImportSection(s) => for i in s { if matches!(i.unwrap().ty, wp::TypeRef::Func(_)) { funcs += 1; } },
            wp::Payload::FunctionSection(s) => { funcs += s.count(); for t in s { func_types.push(t.unwrap()); } }
            _ => {}
        }
    }
    let mut pass = NanCanon { ty32: types, ty64: types + 1, f32_fn: funcs, f64_fn: funcs + 1, sites: 0, inline, param_counts, func_types, cur: 0 };
    let mut module = wasm_encoder::Module::new();
    pass.parse_core_module(&mut module, wp::Parser::new(0), wasm).unwrap();
    eprintln!("nan-canon: {} call sites", pass.sites);
    module.finish()
}

// ---------- inline gas post-pass (run after finite-wasm) ----------
// finite-wasm emits `i64.const N; call 0`. With a *deficit* global d (= -remaining) the charge is
// `global.get d; i64.add; global.set d` -- commutative, so N can stay on the stack -- then trap if d > 0.
struct InlineGas { global: u32 }
impl Reencode for InlineGas {
    type Error = std::convert::Infallible;
    fn parse_global_section(&mut self, globals: &mut wasm_encoder::GlobalSection, section: wp::GlobalSectionReader<'_>) -> Result<(), Error<Self::Error>> {
        utils::parse_global_section(self, globals, section)?;
        assert_eq!(globals.len(), self.global);
        globals.global(wasm_encoder::GlobalType { val_type: ValType::I64, mutable: true, shared: false }, &wasm_encoder::ConstExpr::i64_const(0));
        Ok(())
    }
    fn parse_export_section(&mut self, exports: &mut wasm_encoder::ExportSection, section: wp::ExportSectionReader<'_>) -> Result<(), Error<Self::Error>> {
        utils::parse_export_section(self, exports, section)?;
        exports.export("gas_deficit", wasm_encoder::ExportKind::Global, self.global);
        Ok(())
    }
    fn parse_function_body(&mut self, code: &mut CodeSection, func: wp::FunctionBody<'_>) -> Result<(), Error<Self::Error>> {
        let mut f = self.new_function_with_parsed_locals(&func)?;
        let mut reader = func.get_operators_reader()?;
        while !reader.eof() {
            let op = reader.read()?;
            if matches!(op, wp::Operator::Call { function_index: 0 }) {
                for i in [Instruction::GlobalGet(self.global), Instruction::I64Add, Instruction::GlobalSet(self.global),
                          Instruction::GlobalGet(self.global), Instruction::I64Const(0), Instruction::I64GtS,
                          Instruction::If(wasm_encoder::BlockType::Empty), Instruction::Unreachable, Instruction::End] { f.instruction(&i); }
            } else { f.instruction(&self.instruction(op)?); }
        }
        code.function(&f);
        Ok(())
    }
}
fn inline_gas(wasm: &[u8]) -> Vec<u8> {
    let mut globals = 0;
    for p in wp::Parser::new(0).parse_all(wasm) { if let wp::Payload::GlobalSection(s) = p.unwrap() { globals = s.count(); } }
    let mut module = wasm_encoder::Module::new();
    InlineGas { global: globals }.parse_core_module(&mut module, wp::Parser::new(0), wasm).unwrap();
    module.finish()
}

// ---------- finite-wasm gas + stack ----------
struct Sizes;
impl finite_wasm::max_stack::SizeConfig for Sizes {
    fn size_of_value(&self, ty: wp::ValType) -> u8 {
        match ty { wp::ValType::I32 | wp::ValType::F32 => 4, wp::ValType::I64 | wp::ValType::F64 => 8, wp::ValType::V128 => 16, _ => 32 }
    }
    fn size_of_function_activation(&self, locals: &prefix_sum_vec::PrefixSumVec<wp::ValType, u32>) -> u64 {
        u64::from(locals.max_index().map(|&v| v + 1).unwrap_or(0))
    }
}
macro_rules! define_fee {
    ($( @$proposal:ident $op:ident $({ $($arg:ident: $argty:ty),* })? => $visit:ident ($($ann:tt)*))*) => {
        $( fn $visit(&mut self $($(,$arg: $argty)*)?) -> Self::Output { fee(stringify!($op)) } )*
    }
}
fn fee(op: &str) -> finite_wasm::Fee { if op == "End" { finite_wasm::Fee::ZERO } else { finite_wasm::Fee::constant(1) } }
struct Gas;
impl<'a> wp::VisitOperator<'a> for Gas {
    type Output = finite_wasm::Fee;
    fn simd_visitor(&mut self) -> Option<&mut dyn wp::VisitSimdOperator<'a, Output = Self::Output>> { Some(self) }
    wp::for_each_visit_operator!(define_fee);
}
impl<'a> wp::VisitSimdOperator<'a> for Gas { wp::for_each_visit_simd_operator!(define_fee); }

fn meter(wasm: &[u8]) -> Vec<u8> {
    finite_wasm::Analysis::new().with_stack(Sizes).with_gas(Gas).analyze(wasm).expect("analysis")
        .instrument("meter", wasm).expect("instrument")
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut wasm = std::fs::read(&a[1]).unwrap();
    let t = std::time::Instant::now();
    let has = |k: &str| a[3..].iter().any(|s| s == k);
    if has("nan") { wasm = nan_canon(&wasm, false); }
    if has("nan-inline") { wasm = nan_canon(&wasm, true); }
    if has("gas") || has("gas-inline") { wasm = meter(&wasm); }
    if has("gas-inline") { wasm = inline_gas(&wasm); }
    eprintln!("instrumented in {:?}, {} bytes", t.elapsed(), wasm.len());
    wp::Validator::new_with_features(wp::WasmFeatures::default()).validate_all(&wasm).expect("output must validate");
    std::fs::write(&a[2], wasm).unwrap();
}
