//! Codegen for expression and statement-body lowering: the expression evaluator,
//! binary/logical operators and `if/else`.

use super::{Codegen, Ty, combine_kind, kind_of_type};
use crate::ast::{BinOp, Block, Expr, UnOp};

impl Codegen {
    /// The value kind of an expression (a width+signedness, or `F64`). Drives the
    /// integer-vs-SSE path, the signedness of shift/divide/compare, and the width to
    /// canonicalize to. Arithmetic combines via `combine_kind` (float wins; else the
    /// wider width; else unsigned over signed).
    pub(crate) fn ty_of(&self, e: &Expr) -> Ty {
        match e {
            Expr::Decimal(_) => Ty::F64,
            Expr::Var(n) => self.var_types.get(n).copied().unwrap_or(Ty::I64),
            // Arithmetic carries the kind; comparisons/bitwise/logic are i64 (bool) and
            // fall through to the default below.
            Expr::Binary(BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div, l, r) => {
                combine_kind(self.ty_of(l), self.ty_of(r))
            }
            Expr::Call(callee, _) => {
                if let Expr::Var(name) = callee.as_ref() {
                    self.fn_ret_types.get(name).copied().unwrap_or(Ty::I64)
                } else {
                    Ty::I64
                }
            }
            // Indexing a view yields its ELEMENT type; indexing anything else is a 64-bit word.
            Expr::Index(base, _) => match base.as_ref() {
                Expr::Var(n) => self.view_elem.get(n).copied().unwrap_or(Ty::I64),
                _ => Ty::I64,
            },
            Expr::Unary(UnOp::Neg, inner) => self.ty_of(inner),
            Expr::Cast(_, ty) => kind_of_type(ty),
            Expr::If { then_branch, .. } => {
                then_branch.tail.as_ref().map_or(Ty::I64, |t| self.ty_of(t))
            }
            _ => Ty::I64,
        }
    }

    pub(crate) fn expr(&mut self, expr: &Expr) {
        // Float-typed expressions use the SSE path and leave their result in %xmm0.
        if self.ty_of(expr).is_float() {
            self.fexpr(expr);
            return;
        }
        match expr {
            Expr::Int(n) => self.line(&format!("    movq ${n}, %rax")),
            Expr::Bool(b) => self.line(&format!("    movq ${}, %rax", *b as i64)),
            Expr::Var(name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.line(&format!("    movq {off}(%rbp), %rax"));
                } else if self.global_arrays.contains(name) {
                    // An array global resolves to its base ADDRESS (so `K[i]` indexes).
                    self.line(&format!("    leaq {name}(%rip), %rax"));
                } else if self.global_names.contains(name) {
                    self.line(&format!("    movq {name}(%rip), %rax"));
                } else {
                    panic!("codegen: unknown variable `{name}`");
                }
            }
            Expr::Str(s) => {
                let label = self.intern_string(s);
                self.line(&format!("    leaq {label}(%rip), %rax"));
            }
            Expr::Binary(op, l, r) => self.binary(op, l, r),
            Expr::Call(callee, args) => self.call(callee, args),
            // `base[index]`: an 8-byte word by default, or the view's element width when the
            // base is a typed view. The element width is the whole point of a view: two adjacent
            // u32 slots must be independent, which an 8-byte load would not give.
            Expr::Index(base, index) => {
                let elem = match base.as_ref() {
                    Expr::Var(n) => self.view_elem.get(n).copied(),
                    _ => None,
                };
                self.expr(base);
                self.line("    pushq %rax");
                self.expr(index);
                let shift = match elem.map(|e| e.bits()) {
                    Some(8) => 0,
                    Some(16) => 1,
                    Some(32) => 2,
                    _ => 3,
                };
                if shift > 0 {
                    self.line(&format!("    shlq ${shift}, %rax"));
                }
                self.line("    popq %rcx"); // base
                self.line("    addq %rcx, %rax"); // address
                match elem {
                    // Load exactly the element's bytes, extending by its signedness: an unsigned
                    // element must arrive zero-extended or every later shift and comparison is
                    // wrong for values with the top bit set (an opaque pixel is one).
                    Some(e) => match (e.bits(), e.is_unsigned()) {
                        (8, true) => self.line("    movzbq (%rax), %rax"),
                        (8, false) => self.line("    movsbq (%rax), %rax"),
                        (16, true) => self.line("    movzwq (%rax), %rax"),
                        (16, false) => self.line("    movswq (%rax), %rax"),
                        (32, true) => self.line("    movl (%rax), %eax"), // zero-extends to rax
                        (32, false) => self.line("    movslq (%rax), %rax"),
                        _ => self.line("    movq (%rax), %rax"),
                    },
                    None => self.line("    movq (%rax), %rax"),
                }
            }
            Expr::Unary(op, e) => {
                self.expr(e);
                match op {
                    UnOp::Neg => self.line("    negq %rax"),
                    UnOp::BitNot => self.line("    notq %rax"),
                    UnOp::Not => {
                        // logical not of a 0/1 bool
                        self.line("    cmpq $0, %rax");
                        self.line("    sete %al");
                        self.line("    movzbq %al, %rax");
                    }
                }
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => self.if_else(cond, then_branch, else_branch.as_deref()),
            // `expr as IntType`: a float source is truncated toward zero (cvttsd2si),
            // then the integer result is canonicalized to the target width/sign.
            Expr::Cast(inner, ty) => {
                let target = kind_of_type(ty);
                if self.ty_of(inner).is_float() {
                    self.fexpr(inner); // value in %xmm0
                    self.line("    cvttsd2si %xmm0, %rax");
                } else {
                    self.expr(inner); // value in %rax
                }
                self.canon(target);
            }
            other => todo!("x64 codegen for {other:?}"),
        }
    }

    /// Float (SSE) codegen: emits an `f64` expression leaving its result in `%xmm0`.
    /// The counterpart to `expr` for the SSE class (System V AMD64).
    pub(crate) fn fexpr(&mut self, e: &Expr) {
        match e {
            Expr::Decimal(s) => {
                let bits = s.parse::<f64>().unwrap_or(0.0).to_bits();
                let label = self.intern_float(bits);
                self.line(&format!("    movsd {label}(%rip), %xmm0"));
            }
            Expr::Var(name) => {
                if let Some(&off) = self.locals.get(name) {
                    self.line(&format!("    movsd {off}(%rbp), %xmm0"));
                } else if self.global_names.contains(name) {
                    self.line(&format!("    movsd {name}(%rip), %xmm0"));
                } else {
                    panic!("codegen: unknown float variable `{name}`");
                }
            }
            Expr::Binary(op, l, r) => {
                self.fexpr_operand(l); // left -> %xmm0
                self.line("    subq $16, %rsp");
                self.line("    movsd %xmm0, (%rsp)"); // save left
                self.fexpr_operand(r); // right -> %xmm0
                self.line("    movsd %xmm0, %xmm1"); // right -> %xmm1
                self.line("    movsd (%rsp), %xmm0"); // restore left -> %xmm0
                self.line("    addq $16, %rsp");
                match op {
                    BinOp::Add => self.line("    addsd %xmm1, %xmm0"),
                    BinOp::Sub => self.line("    subsd %xmm1, %xmm0"),
                    BinOp::Mul => self.line("    mulsd %xmm1, %xmm0"),
                    BinOp::Div => self.line("    divsd %xmm1, %xmm0"),
                    other => todo!("float codegen for operator {other:?}"),
                }
            }
            Expr::Call(callee, args) => {
                // A float-returning call: the SSE result is already in %xmm0.
                self.call(callee, args);
            }
            Expr::Unary(UnOp::Neg, inner) => {
                self.fexpr_operand(inner);
                self.line("    movsd %xmm0, %xmm1");
                self.line("    xorpd %xmm0, %xmm0");
                self.line("    subsd %xmm1, %xmm0"); // 0.0 - x
            }
            // `expr as f64`: promote an integer source with cvtsi2sd; a float source
            // is already an f64. (fexpr_operand does exactly this promotion.)
            Expr::Cast(inner, _) => self.fexpr_operand(inner),
            // A float-valued `if`: the condition is integer/bool, each arm's block
            // leaves its float tail in %xmm0 (block() dispatches a float tail to fexpr).
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let else_l = self.fresh_label("felse");
                let end_l = self.fresh_label("fend");
                self.expr(cond); // condition -> %rax (bool)
                self.line("    cmpq $0, %rax");
                self.line(&format!("    je {else_l}"));
                self.block(then_branch); // then -> %xmm0
                self.line(&format!("    jmp {end_l}"));
                self.line(&format!("{else_l}:"));
                match else_branch {
                    Some(eb) => self.block(eb),
                    None => self.line("    xorpd %xmm0, %xmm0"), // no else -> 0.0
                }
                self.line(&format!("{end_l}:"));
            }
            other => todo!("float codegen for {other:?}"),
        }
    }

    /// Evaluates an operand to a float in `%xmm0`, promoting an integer operand with
    /// `cvtsi2sd` (so `2.0 * n` works with `n: i64`).
    fn fexpr_operand(&mut self, e: &Expr) {
        if self.ty_of(e).is_float() {
            self.fexpr(e);
        } else {
            self.expr(e); // integer -> %rax
            self.line("    cvtsi2sdq %rax, %xmm0");
        }
    }

    /// Compares two `f64` operands with `ucomisd`, leaving a 0/1 bool in `%rax`.
    /// Uses the unsigned condition codes ucomisd sets (CF/ZF). (NaN is unordered,
    /// `<`/`>` on a NaN read as this simple mapping; full NaN semantics come with the
    /// type checker.)
    fn fcompare(&mut self, op: &BinOp, l: &Expr, r: &Expr) {
        self.fexpr_operand(l); // left -> %xmm0
        self.line("    subq $16, %rsp");
        self.line("    movsd %xmm0, (%rsp)");
        self.fexpr_operand(r); // right -> %xmm0
        self.line("    movsd %xmm0, %xmm1"); // right -> %xmm1
        self.line("    movsd (%rsp), %xmm0"); // restore left -> %xmm0
        self.line("    addq $16, %rsp");
        self.line("    ucomisd %xmm1, %xmm0"); // flags: %xmm0 (left) vs %xmm1 (right)
        let setcc = match op {
            BinOp::Lt => "setb",
            BinOp::Gt => "seta",
            BinOp::Le => "setbe",
            BinOp::Ge => "setae",
            BinOp::Eq => "sete",
            BinOp::Ne => "setne",
            _ => unreachable!("fcompare only handles comparisons"),
        };
        self.line(&format!("    {setcc} %al"));
        self.line("    movzbq %al, %rax");
    }

    /// Canonicalizes a narrow integer in `%rax` to its 64-bit form: sign-extend
    /// (signed) or zero-extend (unsigned) from the type's width, so `u32`/`i32` values
    /// wrap at 32 bits and are always held sign/zero-extended. A no-op for 64-bit.
    pub(crate) fn canon(&mut self, ty: Ty) {
        match (ty.bits(), ty.is_unsigned()) {
            (8, true) => self.line("    movzbq %al, %rax"),
            (8, false) => self.line("    movsbq %al, %rax"),
            (16, true) => self.line("    movzwq %ax, %rax"),
            (16, false) => self.line("    movswq %ax, %rax"),
            (32, true) => self.line("    movl %eax, %eax"), // zero-extend low 32 -> 64
            (32, false) => self.line("    movslq %eax, %rax"), // sign-extend low 32 -> 64
            _ => {}                                         // 64-bit: already canonical
        }
    }

    /// Interns an `f64` literal by its bit pattern, returning its `.rodata` label.
    pub(crate) fn intern_float(&mut self, bits: u64) -> String {
        let label = format!(".Lflt{}", self.float_consts.len());
        self.float_consts.push((label.clone(), bits));
        label
    }

    /// Interns a string literal, returning its `.rodata` label.
    pub(crate) fn intern_string(&mut self, s: &str) -> String {
        let label = format!(".Lstr{}", self.strings.len());
        self.strings.push((label.clone(), s.to_string()));
        label
    }

    pub(crate) fn binary(&mut self, op: &BinOp, l: &Expr, r: &Expr) {
        // Logical `&&`/`||` short-circuit: the right side is not evaluated when the
        // left already decides the result.
        if matches!(op, BinOp::And | BinOp::Or) {
            self.logical(op, l, r);
            return;
        }
        // A comparison of `f64` operands compares in SSE (ucomisd), the result is
        // still an integer 0/1 bool, so this path stays in the integer world.
        if matches!(
            op,
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne
        ) && (self.ty_of(l).is_float() || self.ty_of(r).is_float())
        {
            self.fcompare(op, l, r);
            return;
        }
        self.expr(l);
        self.line("    pushq %rax");
        self.expr(r);
        self.line("    movq %rax, %rcx");
        self.line("    popq %rax");
        // Signedness selects the right divide/shift/compare instruction, a `u64`
        // operand makes the operation unsigned. This is what retires the `>>`/`>>>`
        // footgun (the shift follows the type) and fixes unsigned divide/compare.
        let usig = self.ty_of(l).is_unsigned() || self.ty_of(r).is_unsigned();
        let ushift = self.ty_of(l).is_unsigned();
        let res = combine_kind(self.ty_of(l), self.ty_of(r));
        // The LEFT operand's own type: a shift follows it, not the combined result type.
        let lty = self.ty_of(l);
        match op {
            BinOp::Add => self.line("    addq %rcx, %rax"),
            BinOp::Sub => self.line("    subq %rcx, %rax"),
            BinOp::Mul => self.line("    imulq %rcx, %rax"),
            // Division/remainder. Signed: sign-extend (cqto) + idivq. Unsigned:
            // zero-extend (xor %rdx) + divq, correct for a u64 that would be
            // "negative" as i64.
            BinOp::Div => {
                if usig {
                    self.line("    xorq %rdx, %rdx");
                    self.line("    divq %rcx");
                } else {
                    self.line("    cqto");
                    self.line("    idivq %rcx");
                }
            }
            BinOp::Mod => {
                if usig {
                    self.line("    xorq %rdx, %rdx");
                    self.line("    divq %rcx");
                } else {
                    self.line("    cqto");
                    self.line("    idivq %rcx");
                }
                self.line("    movq %rdx, %rax");
            }
            BinOp::BitAnd => self.line("    andq %rcx, %rax"),
            BinOp::BitOr => self.line("    orq %rcx, %rax"),
            BinOp::BitXor => self.line("    xorq %rcx, %rax"),
            // Shift count is in %cl. `>>` follows the LEFT operand's type: arithmetic
            // (`sar`) for a signed i64, logical (`shr`) for an unsigned u64, so it is
            // correct by construction. `>>>` stays an explicit logical shift.
            BinOp::Shl => self.line("    shlq %cl, %rax"),
            BinOp::Shr if ushift => self.line("    shrq %cl, %rax"),
            BinOp::Shr => self.line("    sarq %cl, %rax"),
            // `>>>` shifts the VALUE and brings in zeros, so for a narrow type it must shift the
            // narrow value: a signed i8/i16/i32 is held sign-extended in the 64-bit register, and
            // shifting all 64 bits drags those sign bits down into the result. Zero the bits above
            // the type's width first. An unsigned narrow operand is already zero-extended, so this
            // is a no-op for it, and a 64-bit operand has nothing above its width to clear.
            BinOp::LShr => {
                if lty.bits() < 64 && !lty.is_unsigned() {
                    match lty.bits() {
                        8 => self.line("    movzbq %al, %rax"),
                        16 => self.line("    movzwq %ax, %rax"),
                        _ => self.line("    movl %eax, %eax"),
                    }
                }
                self.line("    shrq %cl, %rax")
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                // <,>,<=,>= use different condition codes when unsigned; ==,!= do not.
                let setcc = match (op, usig) {
                    (BinOp::Lt, false) => "setl",
                    (BinOp::Lt, true) => "setb",
                    (BinOp::Gt, false) => "setg",
                    (BinOp::Gt, true) => "seta",
                    (BinOp::Le, false) => "setle",
                    (BinOp::Le, true) => "setbe",
                    (BinOp::Ge, false) => "setge",
                    (BinOp::Ge, true) => "setae",
                    (BinOp::Eq, _) => "sete",
                    (BinOp::Ne, _) => "setne",
                    _ => unreachable!(),
                };
                self.line("    cmpq %rcx, %rax");
                self.line(&format!("    {setcc} %al"));
                self.line("    movzbq %al, %rax");
            }
            other => todo!("x64 codegen for operator {other:?}"),
        }
        // A narrow integer result (i32/u32) wraps at its width, re-canonicalize. Skip
        // comparisons (their result is an i64 bool, already canonical).
        if res.bits() < 64
            && !matches!(
                op,
                BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne
            )
        {
            self.canon(res);
        }
    }

    /// Emits a short-circuiting logical `&&`/`||`, leaving a 0/1 result in `%rax`.
    pub(crate) fn logical(&mut self, op: &BinOp, l: &Expr, r: &Expr) {
        let short = self.fresh_label("sc");
        let end = self.fresh_label("scend");
        self.expr(l);
        self.line("    cmpq $0, %rax");
        match op {
            // `&&`: if the left is false (0), the result is 0, skip the right.
            BinOp::And => self.line(&format!("    je {short}")),
            // `||`: if the left is true (nonzero), the result is 1, skip the right.
            BinOp::Or => self.line(&format!("    jne {short}")),
            _ => unreachable!(),
        }
        // Otherwise the result is whether the right side is nonzero.
        self.expr(r);
        self.line("    cmpq $0, %rax");
        self.line("    setne %al");
        self.line("    movzbq %al, %rax");
        self.line(&format!("    jmp {end}"));
        self.line(&format!("{short}:"));
        match op {
            BinOp::And => self.line("    movq $0, %rax"),
            BinOp::Or => self.line("    movq $1, %rax"),
            _ => unreachable!(),
        }
        self.line(&format!("{end}:"));
    }

    pub(crate) fn if_else(&mut self, cond: &Expr, then_b: &Block, else_b: Option<&Block>) {
        let else_l = self.fresh_label("else");
        let end_l = self.fresh_label("end");
        self.expr(cond);
        self.line("    cmpq $0, %rax");
        self.line(&format!("    je {else_l}"));
        self.block(then_b);
        self.line(&format!("    jmp {end_l}"));
        self.line(&format!("{else_l}:"));
        match else_b {
            Some(eb) => self.block(eb),
            None => self.line("    movq $0, %rax"),
        }
        self.line(&format!("{end_l}:"));
    }
}
