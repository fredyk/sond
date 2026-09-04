//! Codegen for calls: user-function calls and the builtin operations
//! (`print`, `print_int`, memory, UEFI console/keyboard/serial, boot services).

use super::{ARG_REGS, Codegen, MS_ARG_REGS};
use crate::ast::{Arg, Expr};

impl Codegen {
    /// Emits a call. Currently the only builtin with codegen is `write(fd, str)`,
    /// which issues a raw `sys_write` syscall, the first I/O primitive, and the
    /// foundation for a Sond init that talks to the console.
    pub(crate) fn call(&mut self, callee: &Expr, args: &[Arg]) {
        let Expr::Var(name) = callee else {
            todo!("x64 codegen for call of {callee:?}");
        };
        // A call to an `extern fn`: the platform's C ABI, not Sond's. On Windows that is the
        // Microsoft x64 convention — first four arguments in %rcx/%rdx/%r8/%r9, the rest on the
        // stack, and 32 bytes of SHADOW SPACE below them that the CALLER reserves for the callee
        // to spill those four registers into. Skipping the shadow space does not fail loudly: the
        // callee writes over whatever is there, which is the caller's own stack.
        if self.extern_fns.contains(name) {
            let n = args.len();
            let reg = n.min(MS_ARG_REGS.len());
            let stack_n = n - reg;
            // Shadow space (32) + stack arguments, keeping %rsp 16-aligned at the call.
            let bytes = 32 + stack_n as i64 * 8;
            let bytes = bytes + (bytes % 16);
            self.line(&format!("    subq ${bytes}, %rsp"));
            // Stack arguments start ABOVE the shadow space, in order.
            for (i, arg) in args[reg..].iter().enumerate() {
                self.expr(&arg.value);
                self.line(&format!("    movq %rax, {}(%rsp)", 32 + i * 8));
            }
            // Register arguments last, so evaluating them cannot clobber one another.
            for (i, arg) in args[..reg].iter().enumerate() {
                self.expr(&arg.value);
                self.line(&format!("    movq %rax, {}", MS_ARG_REGS[i]));
            }
            self.line(&format!("    call {name}"));
            self.line(&format!("    addq ${bytes}, %rsp"));
            return;
        }
        // A call to another user function: evaluate the arguments, load them into
        // the System V argument registers, and call. Result is left in %rax.
        if self.fn_names.contains(name) {
            let n = args.len();
            let reg = n.min(ARG_REGS.len());
            let stack_n = n - reg; // arguments 7.. go on the stack (System V)
            // Keep %rsp 16-aligned at the call: pad if an odd number of stack args.
            let pad = (stack_n % 2) * 8;
            if pad > 0 {
                self.line("    subq $8, %rsp");
            }
            // Stack args pushed high-to-low so arg7 sits at the lowest address.
            for arg in args[reg..].iter().rev() {
                if let Expr::Var(vname) = &arg.value {
                    if let Some(label) = self.string_rodata_cur.get(vname) {
                        self.line(&format!("    leaq {label}(%rip), %rax"));
                        self.line("    pushq %rax");
                        continue;
                    } else if self.string_vars.contains(vname) {
                        panic!("{}", self.string_unknown_error(vname));
                    }
                }
                self.expr(&arg.value);
                self.line("    pushq %rax");
            }
            // Register args: evaluate, stage on the stack, then pop into registers.
            for arg in &args[..reg] {
                // A tracked local `string` variable → use its UTF-8 rodata
                // label directly, so the callee receives the same bytes it
                // would get from a literal.  An untracked string variable is
                // a compile error: the compiler cannot know what text it
                // holds at runtime.
                if let Expr::Var(vname) = &arg.value {
                    if let Some(label) = self.string_rodata_cur.get(vname) {
                        // Tracked string variable: emit rodata load.
                        self.line(&format!("    leaq {label}(%rip), %rax"));
                        self.line("    pushq %rax");
                        continue;
                    } else if self.string_vars.contains(vname) {
                        // The variable is tracked as a `string` but has no rodata
                        // label in the trace — it came from a function parameter or
                        // some path the compiler cannot prove.  This must be a
                        // compile error.
                        panic!("{}", self.string_unknown_error(vname));
                    }
                }
                self.expr(&arg.value);
                self.line("    pushq %rax");
            }
            for i in (0..reg).rev() {
                self.line(&format!("    popq {}", ARG_REGS[i]));
            }
            self.line(&format!("    call {name}"));
            let cleanup = stack_n * 8 + pad;
            if cleanup > 0 {
                self.line(&format!("    addq ${cleanup}, %rsp"));
            }
            return;
        }
        match name.as_str() {
            // asm("literal"), inline assembly escape hatch: emit the instruction
            // verbatim (AT&T syntax) into the output. The programmer owns register
            // discipline. For what the builtins don't cover: cli/sti/hlt, control
            // registers (cr0/cr3 for paging), model-specific registers, custom
            // port I/O. This is what makes Sond able to write a whole kernel.
            "asm" => {
                assert_eq!(args.len(), 1, "asm expects one string literal");
                let Expr::Str(s) = &args[0].value else {
                    todo!("asm expects a string literal");
                };
                self.line(&format!("    {s}"));
            }
            // addr_of(fn) -> i64: the address of a named function. The building block
            // for interrupt descriptor tables (and, generally, function pointers).
            "addr_of" => {
                assert_eq!(args.len(), 1, "addr_of expects (name)");
                let Expr::Var(n) = &args[0].value else {
                    todo!("addr_of expects a bare function name");
                };
                self.line(&format!("    leaq {n}(%rip), %rax"));
            }
            // lidt(ptr): load the interrupt descriptor table register from a 10-byte
            // pseudo-descriptor at `ptr` (limit u16 then base u64). Kernel-only.
            "lidt" if self.uefi => {
                assert_eq!(args.len(), 1, "lidt expects (ptr)");
                self.expr(&args[0].value); // ptr -> %rax
                self.line("    lidt (%rax)");
            }
            // read_cs() -> i64: the current code-segment selector, needed as the
            // selector field of an IDT gate descriptor.
            "read_cs" if self.uefi => {
                assert!(args.is_empty(), "read_cs takes no arguments");
                self.line("    xorq %rax, %rax");
                self.line("    movw %cs, %ax");
            }
            // swtch(old_ctx_ptr, new_ctx): cooperative context switch. Saves the
            // callee-saved registers and the current %rsp to *old_ctx_ptr, then loads
            // %rsp from new_ctx and restores its callee-saved registers. Returning to
            // a task the first time = bootstrap its stack with a return address to its
            // entry above six register slots. The primitive under a scheduler.
            "swtch" => {
                assert_eq!(args.len(), 2, "swtch expects (old_ctx_ptr, new_ctx)");
                self.expr(&args[0].value); // old_ctx_ptr
                self.line("    pushq %rax");
                self.expr(&args[1].value); // new_ctx
                self.line("    movq %rax, %rsi");
                self.line("    popq %rdi");
                self.line("    call sond_swtch");
                self.uses_swtch = true;
            }
            // write(fd, "literal") -> sys_write(fd, ptr, len); length is known
            // at compile time from the string literal.
            "write" => {
                assert_eq!(args.len(), 2, "write expects (fd, str)");
                let Expr::Str(s) = &args[1].value else {
                    todo!("write's second argument must be a string literal");
                };
                let len = s.len();
                let label = self.intern_string(s);
                self.expr(&args[0].value); // fd -> %rax
                self.line("    movq %rax, %rdi"); // arg1: fd
                self.line(&format!("    leaq {label}(%rip), %rsi")); // arg2: buf
                self.line(&format!("    movq ${len}, %rdx")); // arg3: count
                self.line("    movq $1, %rax"); // sys_write
                self.line("    syscall"); // result (bytes written) in %rax
            }
            // print("literal"), the portable console primitive. On the ELF
            // targets it lowers to write(1, ...); the UEFI target lowers it to
            // ConOut->OutputString (see emit_uefi). Same Sond source, both worlds.
            "print" => {
                assert_eq!(args.len(), 1, "print expects (str)");
                let Expr::Str(s) = &args[0].value else {
                    todo!("print expects a string literal");
                };
                let len = s.len();
                let label = self.intern_string(s);
                if self.uefi {
                    self.uses_serial = true;
                    self.uses_print_serial = true;
                    let kern = self.fresh_label("print_kern");
                    let done = self.fresh_label("print_done");
                    // After ExitBootServices, ConOut is gone: write the string to COM1.
                    self.line("    cmpq $0, sond_ebs_done(%rip)");
                    self.line(&format!("    jne {kern}"));
                    // ConOut->OutputString(ConOut, CHAR16* string), MS x64 ABI.
                    self.line("    movq sond_systab(%rip), %rax"); // SystemTable
                    self.line("    movq 64(%rax), %rcx"); // arg1 = ConOut
                    self.line(&format!("    leaq {label}(%rip), %rdx")); // arg2 = string
                    self.line("    movq 8(%rcx), %rax"); // OutputString fn ptr
                    self.line("    subq $32, %rsp"); // 32-byte shadow space
                    self.line("    call *%rax");
                    self.line("    addq $32, %rsp");
                    self.line(&format!("    jmp {done}"));
                    self.line(&format!("{kern}:")); // kernel mode: same UTF-16 string over COM1
                    self.line(&format!("    leaq {label}(%rip), %rdi"));
                    self.line("    call sond_serial_str");
                    self.line(&format!("{done}:"));
                } else {
                    // write(1, buf, len) via a raw sys_write syscall.
                    self.line("    movq $1, %rdi"); // fd = stdout
                    self.line(&format!("    leaq {label}(%rip), %rsi"));
                    self.line(&format!("    movq ${len}, %rdx"));
                    self.line("    movq $1, %rax"); // sys_write
                    self.line("    syscall");
                }
            }
            // print_int(n), print an integer in decimal via the runtime helper.
            "print_int" => {
                assert_eq!(args.len(), 1, "print_int expects (int)");
                // The argument's type decides the rendering: a SIGNED integer prints with
                // a `-` when negative; an UNSIGNED integer prints its full magnitude. (The
                // type is read before `expr`, which does not change it.)
                let signed = !self.ty_of(&args[0].value).is_unsigned();
                self.expr(&args[0].value); // value -> %rax
                self.line("    movq %rax, %rdi"); // arg1 = value
                if self.uefi {
                    self.line("    call sond_print_int_uefi");
                    self.uses_print_int_uefi = true;
                } else if signed {
                    self.line("    call sond_print_int_signed");
                    self.uses_print_int_signed = true;
                } else {
                    self.line("    call sond_print_int");
                    self.uses_print_int = true;
                }
            }
            // cstr("literal") -> i64: emit the string + NUL terminator in .rodata
            // and return its address. Enables paths and argv for open/execve.
            "cstr" => {
                // `cstr` is gone. A string literal is already a pointer to NUL-terminated
                // bytes, so `cstr("x")` and `"x"` compile to the same thing. Point at the
                // replacement rather than emit code, so the habit does not survive.
                panic!(
                    "`cstr` has been removed: a string literal is already a pointer to \
                     NUL-terminated data, so write the literal directly (`\"x\"` instead of \
                     `cstr(\"x\")`)."
                );
            }
            // argc() -> i64: number of process arguments (hosted; captured in main).
            "argc" => {
                assert!(args.is_empty(), "argc takes no arguments");
                self.line("    movq sond_argc(%rip), %rax");
            }
            // arena_tcb() -> i64: the current thread's arena control block address, held in the
            // reserved arena register %r15. It is per-thread for free (each thread has its own
            // register file), so the allocator bumps through it with no shared global to race on.
            // Three words: [next_free_byte, block_end, total]. main and each spawned thread seed it.
            "arena_tcb" => {
                assert!(args.is_empty(), "arena_tcb takes no arguments");
                self.line("    movq %r15, %rax");
            }
            // argv(i) -> i64: address of the i-th argument (a NUL-terminated string).
            "argv" => {
                assert_eq!(args.len(), 1, "argv expects (index)");
                self.expr(&args[0].value); // index -> %rax
                self.line("    movq sond_argv(%rip), %rcx");
                self.line("    movq (%rcx,%rax,8), %rax");
            }
            // envp(i) -> i64: address of the i-th environment entry ("KEY=VALUE"); NULL-terminated list.
            "envp" => {
                assert_eq!(args.len(), 1, "envp expects (index)");
                self.expr(&args[0].value); // index -> %rax
                self.line("    movq sond_envp(%rip), %rcx");
                self.line("    movq (%rcx,%rax,8), %rax");
            }
            // poweroff() -> reboot(MAGIC1, MAGIC2, CMD_POWER_OFF, 0). Requires
            // CAP_SYS_BOOT (it is a no-op EPERM otherwise), used by a Sond init
            // to cleanly power off the machine/VM instead of panicking on exit.
            "poweroff" => {
                assert!(args.is_empty(), "poweroff takes no arguments");
                self.line("    movl $0xfee1dead, %edi"); // LINUX_REBOOT_MAGIC1
                self.line("    movl $0x28121969, %esi"); // LINUX_REBOOT_MAGIC2
                self.line("    movl $0x4321fedc, %edx"); // LINUX_REBOOT_CMD_POWER_OFF
                self.line("    xorl %r10d, %r10d"); // arg = NULL
                self.line("    movl $169, %eax"); // sys_reboot
                self.line("    syscall");
                self.line("    xorq %rax, %rax"); // returns 0 (if it ever returns)
            }
            // readkey() -> i64 (UEFI): poll ConIn->ReadKeyStroke until a key is
            // available, return its UnicodeChar. ConIn is at offset 48 of the
            // system table; ReadKeyStroke at offset 8 of the input protocol; the
            // EFI_INPUT_KEY has UnicodeChar at offset 2.
            "readkey" if self.uefi => {
                assert!(args.is_empty(), "readkey takes no arguments");
                let poll = self.fresh_label("rk");
                self.line(&format!("{poll}:"));
                self.line("    movq sond_systab(%rip), %rax");
                self.line("    movq 48(%rax), %rcx"); // arg1 = ConIn
                self.line("    leaq sond_key(%rip), %rdx"); // arg2 = &Key
                self.line("    movq 8(%rcx), %rax"); // ReadKeyStroke
                self.line("    subq $32, %rsp");
                self.line("    call *%rax");
                self.line("    addq $32, %rsp");
                self.line("    testq %rax, %rax"); // EFI_SUCCESS?
                self.line(&format!("    jnz {poll}")); // EFI_NOT_READY -> poll again
                self.line("    movzwq sond_key+2(%rip), %rax"); // UnicodeChar -> %rax
            }
            // print_char(c): print one character. UEFI -> ConOut; ELF -> a raw
            // write(1, &byte, 1) so the same source prints on host and freestanding.
            "print_char" => {
                assert_eq!(args.len(), 1, "print_char expects (int)");
                self.expr(&args[0].value); // char code -> %rax
                if self.uefi {
                    self.line("    movw %ax, sond_char(%rip)"); // CHAR16[0] = c
                    self.line("    movw $0, sond_char+2(%rip)"); // NUL terminator
                    self.line("    movq sond_systab(%rip), %rax");
                    self.line("    movq 64(%rax), %rcx"); // arg1 = ConOut
                    self.line("    leaq sond_char(%rip), %rdx"); // arg2 = &string
                    self.line("    movq 8(%rcx), %rax"); // OutputString
                    self.line("    subq $32, %rsp");
                    self.line("    call *%rax");
                    self.line("    addq $32, %rsp");
                } else {
                    self.line("    pushq %rax"); // byte on the stack (low byte first)
                    self.line("    movq $1, %rdi"); // fd = stdout
                    self.line("    movq %rsp, %rsi"); // buf = &byte
                    self.line("    movq $1, %rdx"); // count = 1
                    self.line("    movq $1, %rax"); // sys_write
                    self.line("    syscall");
                    self.line("    popq %rcx"); // restore %rsp
                }
            }
            // alloc(size) -> ptr (UEFI): BootServices->AllocatePool(EFI_LOADER_DATA,
            // size, &buf). BootServices is at system-table offset 96; AllocatePool
            // at offset 64 of the boot-services table. Returns the pool address.
            "alloc" if self.uefi => {
                assert_eq!(args.len(), 1, "alloc expects (size)");
                self.uses_heap = true;
                let kern = self.fresh_label("alloc_kern");
                let done = self.fresh_label("alloc_done");
                self.expr(&args[0].value); // size -> %rax
                // After ExitBootServices, AllocatePool is gone: serve from the bump heap.
                self.line("    cmpq $0, sond_ebs_done(%rip)");
                self.line(&format!("    jne {kern}"));
                self.line("    movq %rax, %rdx"); // arg2 = Size
                self.line("    movq $2, %rcx"); // arg1 = EFI_LOADER_DATA
                self.line("    leaq sond_allocptr(%rip), %r8"); // arg3 = &Buffer
                self.line("    movq sond_systab(%rip), %rax");
                self.line("    movq 96(%rax), %rax"); // BootServices
                self.line("    movq 64(%rax), %rax"); // AllocatePool
                self.line("    subq $32, %rsp");
                self.line("    call *%rax");
                self.line("    addq $32, %rsp");
                self.line("    movq sond_allocptr(%rip), %rax"); // return the pointer
                self.line(&format!("    jmp {done}"));
                self.line(&format!("{kern}:")); // kernel mode: bump allocator
                self.line("    movq %rax, %rdi"); // arg1 = size
                self.line("    call sond_alloc_bump");
                self.line(&format!("{done}:"));
            }
            // Raw memory access (all targets): peek/poke a machine word or byte at
            // a pointer held as an integer. The foundation for buffers and structs.
            "peek64" => {
                assert_eq!(args.len(), 1, "peek64 expects (ptr)");
                self.expr(&args[0].value); // ptr -> %rax
                self.line("    movq (%rax), %rax");
            }
            "peek8" => {
                assert_eq!(args.len(), 1, "peek8 expects (ptr)");
                self.expr(&args[0].value); // ptr -> %rax
                self.line("    movzbq (%rax), %rax");
            }
            "peek16" => {
                assert_eq!(args.len(), 1, "peek16 expects (ptr)");
                self.expr(&args[0].value); // ptr -> %rax
                self.line("    movzwq (%rax), %rax");
            }
            "peek32" => {
                assert_eq!(args.len(), 1, "peek32 expects (ptr)");
                self.expr(&args[0].value); // ptr -> %rax
                self.line("    movl (%rax), %eax"); // 32-bit load zero-extends into %rax
            }
            "poke64" => {
                assert_eq!(args.len(), 2, "poke64 expects (ptr, val)");
                self.expr(&args[0].value); // ptr -> %rax
                self.line("    pushq %rax");
                self.expr(&args[1].value); // val -> %rax
                self.line("    popq %rcx"); // ptr -> %rcx
                self.line("    movq %rax, (%rcx)");
            }
            // `atomic_cas64(ptr, expected, new) -> i64`: compare-and-swap on a 64-bit word.
            // Returns the OLD value at *ptr. If *ptr == expected, writes new and returns old.
            // Uses `lock cmpxchgq` for cross-process atomicity.
            "atomic_cas64" => {
                assert_eq!(args.len(), 3, "atomic_cas64 expects (ptr, expected, new)");
                self.expr(&args[0].value);
                self.line("    pushq %rax");       // push ptr
                self.expr(&args[1].value);
                self.line("    pushq %rax");       // push expected
                self.expr(&args[2].value);
                self.line("    pushq %rax");       // push new
                // stack (top→bottom): new, expected, ptr
                self.line("    movq (%rsp), %r12");     // r12 = new
                self.line("    movq 8(%rsp), %rax");    // rax = expected
                self.line("    movq 16(%rsp), %r11");   // r11 = ptr
                self.line("    lock cmpxchgq %r12, (%r11)");
                self.line("    addq $24, %rsp");        // restore 3 pushes
            }
            // `atomic_add64(ptr, delta) -> i64`: fetch-and-add on a 64-bit word.
            // Returns the OLD value at ptr, then sets *ptr = old + delta.
            // Uses `lock xaddq` for cross-process atomicity.
            "atomic_add64" => {
                assert_eq!(args.len(), 2, "atomic_add64 expects (ptr, delta)");
                self.expr(&args[0].value); // ptr -> %rax
                self.line("    pushq %rax"); // save ptr on stack
                self.expr(&args[1].value); // delta -> %rax
                self.line("    pushq %rax"); // save delta on stack
                self.line("    movq 8(%rsp), %rax"); // ptr -> %rax
                self.line("    movq (%rsp), %rcx");  // delta -> %rcx
                self.line("    lock xaddq %rcx, (%rax)"); // *ptr += delta; old = %rcx
                self.line("    movq %rcx, %rax");      // return old value
                self.line("    addq $16, %rsp");       // restore stack
            }
            // poke8(v): write one byte at *ptr.
            "poke8" => {
                assert_eq!(args.len(), 2, "poke8 expects (ptr, val)");
                self.expr(&args[0].value); // ptr -> %rax
                self.line("    pushq %rax");
                self.expr(&args[1].value); // val -> %rax
                self.line("    popq %rcx"); // ptr -> %rcx
                self.line("    movb %al, (%rcx)");
            }
            "poke16" => {
                assert_eq!(args.len(), 2, "poke16 expects (ptr, val)");
                self.expr(&args[0].value); // ptr -> %rax
                self.line("    pushq %rax");
                self.expr(&args[1].value); // val -> %rax
                self.line("    popq %rcx"); // ptr -> %rcx
                self.line("    movw %ax, (%rcx)");
            }
            "poke32" => {
                assert_eq!(args.len(), 2, "poke32 expects (ptr, val)");
                self.expr(&args[0].value); // ptr -> %rax
                self.line("    pushq %rax");
                self.expr(&args[1].value); // val -> %rax
                self.line("    popq %rcx"); // ptr -> %rcx
                self.line("    movl %eax, (%rcx)");
            }
            // serial_putc(c): write one byte to COM1 (0x3F8) via port I/O. Ring-0
            // only (UEFI/kernel), but it needs NO firmware, it survives
            // ExitBootServices, unlike ConOut.
            "serial_putc" if self.uefi => {
                assert_eq!(args.len(), 1, "serial_putc expects (byte)");
                self.expr(&args[0].value); // byte -> %rax
                self.line("    movq %rax, %rdi");
                self.line("    call sond_serial_putc");
                self.uses_serial = true;
            }
            // serial_print("literal"): write an ASCII string + CRLF to COM1,
            // unrolled to per-byte writes. The kernel's console after boot services.
            "serial_print" if self.uefi => {
                assert_eq!(args.len(), 1, "serial_print expects (str)");
                let Expr::Str(s) = &args[0].value else {
                    todo!("serial_print expects a string literal");
                };
                for b in s.bytes().chain([13, 10]) {
                    self.line(&format!("    movq ${b}, %rdi"));
                    self.line("    call sond_serial_putc");
                }
                self.uses_serial = true;
            }
            // exit_boot_services(): the firmware hands the machine to Sond. Disable
            // the watchdog, get the current memory map (its key is required),
            // then ExitBootServices(ImageHandle, MapKey), retrying if the key went
            // stale. After this, Boot Services (ConOut, alloc, ...) are gone: only
            // direct hardware (serial_*, port I/O, memory) works. Sond is a kernel.
            "exit_boot_services" if self.uefi => {
                assert!(args.is_empty(), "exit_boot_services takes no arguments");
                self.uses_heap = true;
                let retry = self.fresh_label("ebs");
                // SetWatchdogTimer(0, 0, 0, 0), stop the 5-min firmware reset.
                self.line("    movq sond_systab(%rip), %rax");
                self.line("    movq 96(%rax), %rax"); // BootServices
                self.line("    movq 256(%rax), %r11"); // SetWatchdogTimer
                self.line("    xorq %rcx, %rcx");
                self.line("    xorq %rdx, %rdx");
                self.line("    xorq %r8, %r8");
                self.line("    xorq %r9, %r9");
                self.line("    subq $32, %rsp");
                self.line("    call *%r11");
                self.line("    addq $32, %rsp");
                // Probe: GetMemoryMap(&size=0, NULL, &key, &dsize, &dver).
                self.line("    movq $0, sond_mmap_size(%rip)");
                self.line("    movq sond_systab(%rip), %rax");
                self.line("    movq 96(%rax), %rax");
                self.line("    movq 56(%rax), %r11"); // GetMemoryMap
                self.line("    leaq sond_mmap_size(%rip), %rcx");
                self.line("    xorq %rdx, %rdx");
                self.line("    leaq sond_mmap_key(%rip), %r8");
                self.line("    leaq sond_mmap_dsize(%rip), %r9");
                self.line("    subq $48, %rsp"); // 32 shadow + 8 stack arg + 8 align
                self.line("    leaq sond_mmap_dver(%rip), %rax");
                self.line("    movq %rax, 32(%rsp)"); // 5th arg = &DescVer
                self.line("    call *%r11");
                self.line("    addq $48, %rsp");
                // Allocate the map buffer with slack (AllocatePool grows the map).
                self.line("    movq sond_mmap_size(%rip), %rax");
                self.line("    addq $1024, %rax");
                self.line("    movq %rax, sond_mmap_cap(%rip)");
                self.line("    movq %rax, %rdx"); // Size
                self.line("    movq $2, %rcx"); // EFI_LOADER_DATA
                self.line("    leaq sond_mmap_buf(%rip), %r8");
                self.line("    movq sond_systab(%rip), %rax");
                self.line("    movq 96(%rax), %rax");
                self.line("    movq 64(%rax), %rax"); // AllocatePool
                self.line("    subq $32, %rsp");
                self.line("    call *%rax");
                self.line("    addq $32, %rsp");
                // retry: fresh GetMemoryMap into the buffer, then ExitBootServices.
                self.line(&format!("{retry}:"));
                self.line("    movq sond_mmap_cap(%rip), %rax");
                self.line("    movq %rax, sond_mmap_size(%rip)"); // reset in/out size
                self.line("    movq sond_systab(%rip), %rax");
                self.line("    movq 96(%rax), %rax");
                self.line("    movq 56(%rax), %r11");
                self.line("    leaq sond_mmap_size(%rip), %rcx");
                self.line("    movq sond_mmap_buf(%rip), %rdx");
                self.line("    leaq sond_mmap_key(%rip), %r8");
                self.line("    leaq sond_mmap_dsize(%rip), %r9");
                self.line("    subq $48, %rsp");
                self.line("    leaq sond_mmap_dver(%rip), %rax");
                self.line("    movq %rax, 32(%rsp)");
                self.line("    call *%r11");
                self.line("    addq $48, %rsp");
                self.line("    movq sond_systab(%rip), %rax");
                self.line("    movq 96(%rax), %rax");
                self.line("    movq 232(%rax), %r11"); // ExitBootServices
                self.line("    movq sond_imghandle(%rip), %rcx");
                self.line("    movq sond_mmap_key(%rip), %rdx");
                self.line("    subq $32, %rsp");
                self.line("    call *%r11");
                self.line("    addq $32, %rsp");
                self.line("    testq %rax, %rax");
                self.line(&format!("    jnz {retry}")); // stale key -> retry
                // Firmware is gone: seed the kernel's own heap from the captured map,
                // so alloc() keeps working (sets sond_ebs_done, heap_ptr, heap_end).
                self.line("    call sond_heap_init");
                self.line("    xorq %rax, %rax");
            }
            // serial_int(n): print an integer in decimal over COM1. Works after
            // ExitBootServices, when print_int (ConOut) is dead, the kernel's way
            // to show numbers (counters, addresses, sizes).
            "serial_int" if self.uefi => {
                assert_eq!(args.len(), 1, "serial_int expects (int)");
                self.expr(&args[0].value); // value -> %rax
                self.line("    movq %rax, %rdi");
                self.line("    call sond_serial_int");
                self.uses_serial = true;
                self.uses_serial_int = true;
            }
            // halt(): stop the CPU forever (hlt in a loop). How a kernel ends,
            // nothing to return to once boot services are gone.
            "halt" if self.uefi => {
                assert!(args.is_empty(), "halt takes no arguments");
                let loop_l = self.fresh_label("halt");
                self.line(&format!("{loop_l}:"));
                self.line("    hlt");
                self.line(&format!("    jmp {loop_l}"));
            }
            // Typed port I/O, the bread and butter of device drivers. First-class
            // so a driver need not drop to asm() for every register access.
            // outb(port, val) / outw(port, val): write a byte/word to an I/O port.
            "outb" | "outw" | "outl" => {
                assert_eq!(args.len(), 2, "{name} expects (port, val)");
                self.expr(&args[0].value); // port -> %rax
                self.line("    pushq %rax");
                self.expr(&args[1].value); // val -> %rax
                self.line("    popq %rdx"); // port -> %rdx (only %dx used)
                match name.as_str() {
                    "outb" => self.line("    outb %al, %dx"),
                    "outw" => self.line("    outw %ax, %dx"),
                    _ => self.line("    outl %eax, %dx"),
                }
            }
            // inb/inw/inl(port) -> i64: read a byte/word/dword from an I/O port.
            // The 32-bit `inl` writes %eax, which zero-extends into %rax on x86-64,
            // so the returned value is already the clean unsigned dword.
            "inb" | "inw" | "inl" => {
                assert_eq!(args.len(), 1, "{name} expects (port)");
                self.expr(&args[0].value); // port -> %rax
                self.line("    movq %rax, %rdx"); // port -> %dx
                self.line("    xorq %rax, %rax");
                match name.as_str() {
                    "inb" => self.line("    inb %dx, %al"),
                    "inw" => self.line("    inw %dx, %ax"),
                    _ => self.line("    inl %dx, %eax"),
                }
            }
            // syscall(num, a1, a2, a3, a4, a5, a6) -> i64: raw Linux x86-64 syscall.
            // ABI: num->%rax, a1..a6->%rdi,%rsi,%rdx,%r10,%r8,%r9; missing = 0.
            "syscall" => {
                let n = args.len();
                assert!(
                    (1..=7).contains(&n),
                    "syscall expects 1 to 7 arguments, got {n}"
                );
                for arg in args {
                    self.expr(&arg.value);
                    self.line("    pushq %rax");
                }
                for _ in n..7 {
                    self.line("    pushq $0");
                }
                self.line("    popq %r9");
                self.line("    popq %r8");
                self.line("    popq %r10");
                self.line("    popq %rdx");
                self.line("    popq %rsi");
                self.line("    popq %rdi");
                self.line("    popq %rax");
                self.line("    syscall");
            }
            other => todo!("x64 codegen for builtin/function `{other}`"),
        }
    }
}
