//! Codegen for the runtime support routines emitted alongside the program:
//! the integer-printing helpers, the serial (COM1) helpers, and the read-only
//! string data.

use super::Codegen;

impl Codegen {
    /// Emits the `sond_print_int` runtime helper (ELF targets): converts the
    /// non-negative integer in `%rdi` to decimal ASCII and writes it to fd 1.
    pub(crate) fn emit_print_int_helper(&mut self) {
        if !self.uses_print_int {
            return;
        }
        for line in [
            ".text",
            "sond_print_int:",
            "    pushq %rbp",
            "    movq %rsp, %rbp",
            "    subq $32, %rsp",
            "    movq %rdi, %rax", // value
            "    movq %rbp, %rsi", // rsi = one past the (downward) buffer
            "    movq $10, %rcx",  // divisor
            "    testq %rax, %rax",
            "    jnz .Lpi_loop",
            "    decq %rsi", // value == 0 -> emit '0'
            "    movb $48, (%rsi)",
            "    jmp .Lpi_write",
            ".Lpi_loop:",
            "    testq %rax, %rax",
            "    jz .Lpi_write",
            "    xorq %rdx, %rdx",
            "    divq %rcx",     // rax /= 10, rdx = remainder
            "    addb $48, %dl", // digit -> ASCII
            "    decq %rsi",
            "    movb %dl, (%rsi)",
            "    jmp .Lpi_loop",
            ".Lpi_write:",
            "    movq %rbp, %rdx",
            "    subq %rsi, %rdx", // rdx = number of digits
            "    movq $1, %rax",   // sys_write
            "    movq $1, %rdi",   // fd = stdout
            "    syscall",         // rsi already points at the first digit
            "    movq %rbp, %rsp",
            "    popq %rbp",
            "    ret",
        ] {
            self.line(line);
        }
    }

    /// Emits `sond_print_int_signed` (ELF targets): prints the SIGNED integer in `%rdi`
    /// in decimal. A negative value gets a leading `-` and its magnitude; the magnitude is
    /// computed with `negq` then printed unsigned, which is correct even for i64::MIN
    /// (whose `negq` is itself, and whose unsigned reading equals |MIN|). Non-negative
    /// values print as usual. This is the type-aware counterpart to `sond_print_int`
    /// (used for unsigned types), so a computed `-5` shows `-5`, not `18446744073709551611`.
    pub(crate) fn emit_print_int_signed_helper(&mut self) {
        if !self.uses_print_int_signed {
            return;
        }
        for line in [
            ".text",
            "sond_print_int_signed:",
            "    pushq %rbp",
            "    movq %rsp, %rbp",
            "    subq $32, %rsp",
            "    movq %rdi, %rax", // value
            "    movq %rbp, %rsi", // rsi = one past the (downward) buffer
            "    movq $10, %rcx",  // divisor
            "    xorq %r8, %r8",   // r8 = negative flag (0 = non-negative)
            "    testq %rax, %rax",
            "    jns .Lpis_mag",  // >= 0 -> print magnitude directly
            "    movq $1, %r8",   // negative: remember the sign
            "    negq %rax",      // magnitude (MIN -> MIN; its unsigned reading is |MIN|)
            ".Lpis_mag:",
            "    testq %rax, %rax",
            "    jnz .Lpis_loop",
            "    decq %rsi", // value == 0 -> emit '0'
            "    movb $48, (%rsi)",
            "    jmp .Lpis_sign",
            ".Lpis_loop:",
            "    testq %rax, %rax",
            "    jz .Lpis_sign",
            "    xorq %rdx, %rdx",
            "    divq %rcx",     // rax /= 10 (unsigned on the magnitude), rdx = remainder
            "    addb $48, %dl", // digit -> ASCII
            "    decq %rsi",
            "    movb %dl, (%rsi)",
            "    jmp .Lpis_loop",
            ".Lpis_sign:",
            "    testq %r8, %r8",
            "    jz .Lpis_write",
            "    decq %rsi", // prepend '-'
            "    movb $45, (%rsi)",
            ".Lpis_write:",
            "    movq %rbp, %rdx",
            "    subq %rsi, %rdx", // rdx = number of characters
            "    movq $1, %rax",   // sys_write
            "    movq $1, %rdi",   // fd = stdout
            "    syscall",         // rsi already points at the first character
            "    movq %rbp, %rsp",
            "    popq %rbp",
            "    ret",
        ] {
            self.line(line);
        }
    }

    /// Emits the `sond_print_int_uefi` helper: converts the non-negative integer
    /// in `%rdi` to a UTF-16 decimal string in the `sond_intbuf` scratch (filled
    /// backwards, NUL-terminated) and prints it via ConOut->OutputString.
    pub(crate) fn emit_print_int_uefi_helper(&mut self) {
        if !self.uses_print_int_uefi {
            return;
        }
        for line in [
            ".text",
            "sond_print_int_uefi:",
            "    pushq %rbp",
            "    movq %rsp, %rbp",
            "    subq $32, %rsp",
            "    movq %rdi, %rax",                 // value
            "    leaq sond_intbuf+46(%rip), %rsi", // rsi = &intbuf[23] (terminator)
            "    movw $0, (%rsi)",                 // CHAR16 NUL
            "    movq $10, %rcx",                  // divisor
            "    testq %rax, %rax",
            "    jnz .Lpiu_loop",
            "    subq $2, %rsi", // value == 0 -> emit '0'
            "    movw $48, (%rsi)",
            "    jmp .Lpiu_write",
            ".Lpiu_loop:",
            "    testq %rax, %rax",
            "    jz .Lpiu_write",
            "    xorq %rdx, %rdx",
            "    divq %rcx",      // rax /= 10, rdx = remainder
            "    addq $48, %rdx", // digit -> ASCII
            "    subq $2, %rsi",  // step one CHAR16 back
            "    movw %dx, (%rsi)",
            "    jmp .Lpiu_loop",
            ".Lpiu_write:",
            "    movq sond_systab(%rip), %rax",
            "    movq 64(%rax), %rcx", // arg1 = ConOut
            "    movq %rsi, %rdx",     // arg2 = &string (first digit)
            "    movq 8(%rcx), %rax",  // OutputString
            "    subq $32, %rsp",
            "    call *%rax",
            "    addq $32, %rsp",
            "    movq %rbp, %rsp",
            "    popq %rbp",
            "    ret",
        ] {
            self.line(line);
        }
    }

    /// Emits `sond_serial_putc` (byte in `%rdi`): polls the 16550 UART line-status
    /// register (0x3FD, bit 5 = transmit-holding empty) and writes the byte to the
    /// data register (0x3F8). Pure port I/O, no firmware, works after boot services.
    pub(crate) fn emit_serial_helper(&mut self) {
        if !self.uses_serial {
            return;
        }
        for line in [
            ".text",
            "sond_serial_putc:",
            "    movq %rdi, %rcx", // save the byte
            ".Lserw:",
            "    movw $0x3FD, %dx", // Line Status Register
            "    inb %dx, %al",
            "    testb $0x20, %al", // THR empty?
            "    jz .Lserw",
            "    movw $0x3F8, %dx", // data register
            "    movb %cl, %al",
            "    outb %al, %dx",
            "    ret",
        ] {
            self.line(line);
        }
    }

    /// Emits `sond_serial_int` (integer in `%rdi`): converts to decimal ASCII in
    /// the `sond_intbuf` scratch (filled backwards) and writes each digit to COM1
    /// via `sond_serial_putc`. The kernel's `print_int` after boot services.
    pub(crate) fn emit_serial_int_helper(&mut self) {
        if !self.uses_serial_int {
            return;
        }
        for line in [
            ".text",
            "sond_serial_int:",
            "    pushq %rbp",
            "    movq %rsp, %rbp",
            "    subq $32, %rsp",
            "    movq %rdi, %rax",                 // value
            "    leaq sond_intbuf+40(%rip), %rsi", // write ASCII backwards from here
            "    movq %rsi, %r8",                  // r8 = one-past-last-digit
            "    movq $10, %rcx",
            "    testq %rax, %rax",
            "    jnz .Lsi_loop",
            "    decq %rsi", // value == 0 -> single '0'
            "    movb $48, (%rsi)",
            "    jmp .Lsi_emit",
            ".Lsi_loop:",
            "    testq %rax, %rax",
            "    jz .Lsi_emit",
            "    xorq %rdx, %rdx",
            "    divq %rcx",
            "    addb $48, %dl",
            "    decq %rsi",
            "    movb %dl, (%rsi)",
            "    jmp .Lsi_loop",
            ".Lsi_emit:", // write bytes [rsi, r8) to COM1
            "    cmpq %r8, %rsi",
            "    jae .Lsi_done",
            "    movzbq (%rsi), %rdi",
            "    incq %rsi",
            "    call sond_serial_putc", // preserves %rsi and %r8
            "    jmp .Lsi_emit",
            ".Lsi_done:",
            "    movq %rbp, %rsp",
            "    popq %rbp",
            "    ret",
        ] {
            self.line(line);
        }
    }

    /// Emits the post-ExitBootServices heap: `sond_heap_init` seeds a bump
    /// allocator from the captured UEFI memory map (the largest EfiConventionalMemory
    /// region above 1 MiB, free RAM the firmware wasn't using, so it can't clobber
    /// our image, stack, or the map buffer), and `sond_alloc_bump` serves from it.
    /// This is what lets `alloc` (hence virtio-net, TCP and SSH) keep working once
    /// the firmware (AllocatePool) is gone: the kernel owns memory now.
    pub(crate) fn emit_heap_helpers(&mut self) {
        if !self.uses_heap {
            return;
        }
        for line in [
            ".text",
            // sond_heap_init(): scan the memory map, pick the biggest type-7 region.
            "sond_heap_init:",
            "    movq sond_mmap_buf(%rip), %rsi", // cur descriptor
            "    movq %rsi, %r10",
            "    addq sond_mmap_size(%rip), %r10", // one-past-end
            "    xorq %r8, %r8",                   // best_start
            "    xorq %r9, %r9",                   // best_len
            ".Lhi_loop:",
            "    cmpq %r10, %rsi",
            "    jae .Lhi_done",
            "    movl (%rsi), %eax", // Type (u32)
            "    cmpl $7, %eax",     // EfiConventionalMemory
            "    jne .Lhi_next",
            "    movq 8(%rsi), %rcx",   // PhysicalStart
            "    cmpq $0x100000, %rcx", // skip low memory (< 1 MiB)
            "    jb .Lhi_next",
            "    movq 24(%rsi), %rdx", // NumberOfPages
            "    shlq $12, %rdx",      // * 4096 -> bytes
            "    cmpq %r9, %rdx",
            "    jbe .Lhi_next",
            "    movq %rcx, %r8", // new best
            "    movq %rdx, %r9",
            ".Lhi_next:",
            "    addq sond_mmap_dsize(%rip), %rsi",
            "    jmp .Lhi_loop",
            ".Lhi_done:",
            "    movq %r8, %rax",
            "    addq %r9, %rax", // end = start + len
            "    movq %rax, sond_heap_end(%rip)",
            "    addq $15, %r8", // align start up to 16
            "    andq $-16, %r8",
            "    movq %r8, sond_heap_ptr(%rip)",
            "    movq $1, sond_ebs_done(%rip)",
            "    ret",
            // sond_alloc_bump(size in %rdi) -> ptr in %rax (0 on OOM).
            "sond_alloc_bump:",
            "    movq %rdi, %rcx",
            "    addq $15, %rcx", // round size up to 16
            "    andq $-16, %rcx",
            "    movq sond_heap_ptr(%rip), %rax", // ptr to return
            "    movq %rax, %rdx",
            "    addq %rcx, %rdx", // new cursor
            "    jc .Lab_oom",     // address overflow
            "    cmpq sond_heap_end(%rip), %rdx",
            "    ja .Lab_oom",
            "    movq %rdx, sond_heap_ptr(%rip)",
            "    ret",
            ".Lab_oom:",
            "    xorq %rax, %rax",
            "    ret",
        ] {
            self.line(line);
        }
    }

    /// Emits `sond_serial_str` (UTF-16 NUL-terminated string in `%rdi`): writes each
    /// unit's low byte to COM1 via `sond_serial_putc`. This is how `print` keeps
    /// working after ExitBootServices, when ConOut is dead, the same interned CHAR16
    /// string the firmware path uses, now going to the serial port instead.
    pub(crate) fn emit_serial_str_helper(&mut self) {
        if !self.uses_print_serial {
            return;
        }
        for line in [
            ".text",
            "sond_serial_str:",
            "    pushq %rbp",
            "    movq %rsp, %rbp",
            "    pushq %rbx",
            "    movq %rdi, %rbx", // cur CHAR16 ptr
            ".Lstr_loop:",
            "    movzwq (%rbx), %rax", // next CHAR16
            "    testq %rax, %rax",
            "    jz .Lstr_done",         // NUL terminator
            "    movzbq %al, %rdi",      // low byte -> arg
            "    call sond_serial_putc", // clobbers rax/rcx/rdx, preserves rbx
            "    addq $2, %rbx",
            "    jmp .Lstr_loop",
            ".Lstr_done:",
            "    popq %rbx",
            "    popq %rbp",
            "    ret",
        ] {
            self.line(line);
        }
    }

    /// Emits `sond_swtch(old_ctx_ptr in %rdi, new_ctx in %rsi)`: the cooperative
    /// context switch. Saves the six callee-saved registers and %rsp into
    /// *old_ctx_ptr, loads %rsp from new_ctx, restores its callee-saved registers,
    /// and returns, so control resumes wherever the new context last called swtch
    /// (or, for a freshly bootstrapped stack, at the return address planted above its
    /// six saved-register slots). The heart of a scheduler.
    pub(crate) fn emit_swtch_helper(&mut self) {
        if !self.uses_swtch {
            return;
        }
        for line in [
            ".text",
            "sond_swtch:",
            "    pushq %rbp",
            "    pushq %rbx",
            "    pushq %r12",
            "    pushq %r13",
            "    pushq %r14",
            "    pushq %r15",
            "    movq %rsp, (%rdi)", // *old_ctx_ptr = current rsp
            "    movq %rsi, %rsp",   // rsp = new_ctx
            "    popq %r15",
            "    popq %r14",
            "    popq %r13",
            "    popq %r12",
            "    popq %rbx",
            "    popq %rbp",
            "    ret",
        ] {
            self.line(line);
        }
    }

    /// Emits the string-literal data. On UEFI each string is UTF-16 (CHAR16),
    /// as ConOut expects; on the ELF targets it is raw bytes for `write`.
    pub(crate) fn emit_rodata(&mut self) {
        // `f64` literals: 8-byte IEEE-754 doubles in read-only data, all targets.
        if !self.float_consts.is_empty() {
            let floats = std::mem::take(&mut self.float_consts);
            self.line(".section .rodata");
            for (label, bits) in &floats {
                self.line(&format!("{label}:"));
                self.line(&format!("    .quad {bits}"));
            }
        }
        if self.strings.is_empty() {
            return;
        }
        let strings = std::mem::take(&mut self.strings);
        if self.uefi {
            // Note: efi_main already emitted `.data` for sond_systab.
            for (label, bytes) in &strings {
                let mut units: Vec<String> = bytes
                    .chars()
                    .map(|c| (c as u32 & 0xffff).to_string())
                    .collect();
                units.push("13".to_string()); // \r
                units.push("10".to_string()); // \n
                units.push("0".to_string()); // NUL
                self.line(&format!("{label}:\n    .short {}", units.join(", ")));
            }
        } else {
            self.line(".section .rodata");
            for (label, bytes) in &strings {
                let escaped: String = bytes
                    .chars()
                    .map(|c| match c {
                        '"' => "\\\"".to_string(),
                        '\\' => "\\\\".to_string(),
                        '\n' => "\\n".to_string(),
                        '\t' => "\\t".to_string(),
                        other => other.to_string(),
                    })
                    .collect();
                self.line(&format!("{label}:"));
                self.line(&format!("    .asciz \"{escaped}\""));
            }
        }
    }
}
