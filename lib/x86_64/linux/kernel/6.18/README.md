<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Jhon Magdalena Vila and the Sond contributors -->

# Sond Linux 6.18-sond

Kernel de Linux 6.18 reescrito en [Sond](https://github.com/Sond/), una lengua
de programacion escrita por IA para escribir IA. Arranca en **UEFI desnudo**
(ExitBootServices), con la estructura de subsistemas de Linux (arch/x86, mm,
kernel, drivers, init), creciendo subsistema a subsistema.

## Estado

| Estado | Subsistema | Notas |
|--------|-----------|-------|
| DONE | init | `main.snd`, `efi_main()`, `start_kernel()` idle loop, timer ISR, tick output via 8250 |
| DONE | arch/x86/gdt | GDT: null, kcode, kdata, udata. Carga con `lgdt`, sin far-jump |
| DONE | arch/x86/idt | IDT gates, 8259 PIC remap, PIT program |
| DONE | drivers/tty/serial | 8250 driver (`8250.snd`), printk subsystem (`printk.snd`) |
| TODO | mm | Page allocator, physical memory manager, virtual memory |
| TODO | kernel | task_struct, scheduling, process management |
| TODO | drivers | Console, keyboard, virtio-net |
| TODO | fs | Minimal filesystem (procfs, tmpfs) |

## Compilacion

```bash
# Cada fichero compila por si mismo, sin dependencias cruzadas de build system.
sondc lib/x86_64/linux/kernel/6.18/init/main.snd --uefi -o BOOTX64.EFI

# Ver public API sin compilar:
sondc lib/x86_64/linux/kernel/6.18/arch/x86/gdt.snd --emit-header
sondc lib/x86_64/linux/kernel/6.18/arch/x86/idt.snd --emit-header
```

## Boot (QEMU/VirtualBox)

```bash
# QEMU UEFI (OVMF):
qemu-system-x86_64 \
  -bios /usr/share/OVMF/OVMF_CODE.fd \
  -drive file=BOOTX64.EFI,format=raw,if=mtd  # o montar EFI vars partition

# Alternativamente (con grub):
qemu-system-x86_64 \
  -bios /usr/share/OVMF/OVMF_CODE.fd \
  -drive file=kernel.iso,format=raw
```

El kernel imprime por serie COM1:

```
Sond Linux 6.18-sond booting...
[    0.000000] Exiting boot services...
[    0.000000] Boot services exited.
[    0.000000] Sond/Linux 6.18-sond
[    0.000000] Memory: available
[    0.000000] CPU: 1 core, 64-bit mode
[    0.000000] Serial console (8250 COM1) ready.
[    0.000000] Initializing GDT...
[    0.000000] GDT loaded.
[    0.000000] Initializing IDT...
[    0.000000] IDT loaded.
[    0.000000] Remapping PIC...
[    0.000000] PIC remapped.
[    0.000000] Initializing PIT (100 Hz)...
[    0.000000] PIT initialized.
[    0.000000] Interrupts enabled.
[    0.000000] Sond/Linux 6.18-sond booted.
[    0.000000] Entering idle loop.
[    0.000000] tick jiffies=100
[    0.000000] tick jiffies=200
[    0.000000] tick jiffies=300
[    0.000000] tick jiffies=400
[    0.000000] tick jiffies=500
[    0.000000] Kernel idle loop complete.
```

## Estructura

```
lib/x86_64/linux/kernel/6.18/
├── arch/x86/
│   ├── gdt.snd    # Global Descriptor Table
│   └── idt.snd    # Interrupt Descriptor Table + PIC + PIT
├── init/
│   └── main.snd   # Entry point, start_kernel()
├── kernel/        # task_struct, scheduler, process management
├── mm/            # Memory management, page allocator
└── drivers/       # Console, keyboard, network, virtio
```

## Crecimiento (siguientes pasos)

1. `mm/page_alloc.snd`, Physical memory allocator (bitmap del mapa de memoria de UEFI)
2. `mm/vm.snd`, Page tables, `pgd`, virtual memory
3. `kernel/task_struct.snd`, `struct task_struct`, `thread_info`
4. `kernel/sched.snd`, `swtch()`, round-robin scheduler
5. `drivers/serial.snd`, Serial console driver (mejora de la primitiva cruda)
6. `kernel/syscall.snd`, sysenter/syscall, system call tables