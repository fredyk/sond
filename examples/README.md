# Sond examples

Programs written in Sond, organized as `<target>/<theme>/<name>/<name>.snd`.
`host/*` are hosted ELF programs (`sondc x.snd`); `uefi/*` boot on bare firmware
(`sondc x.snd --uefi`) and run in QEMU/OVMF; `freestanding/*` are libc-free `/init`.

Almost every example was written by an isolated qwen3.6 worker that knew only the
[language reference](../docs/implemented-language-surface.md), researched the
relevant spec online, and compiled (and, where the output is verifiable, ran and
checked) its own code before delivering it. Each example lives in its own subfolder.

Compile and run a hosted example:
```sh
sondc examples/host/crypto/rc4/rc4.snd -o /tmp/rc4 && /tmp/rc4
```

## The OS tower (RFC-0003 kernel/uefi profile)

**Phase 1, the full language on bare UEFI**
- `uefi/console/hello`, `uefi/language/{logic,loops,numbers,squares,countdown,fibonacci,evens,maxof3,multab,sumfirst,triangle}`, variables, arithmetic, if/else, loops, functions on the metal.

**Phase 2, Boot Services**
- `uefi/memory/{heap,memsum,ringbuf}`, AllocatePool heap, peek/poke round-trips.
- `uefi/input/{keycheck,shell,repl}`, ConIn keyboard → an interactive mini-shell.

**Phase 3, a real kernel**
- `uefi/kernel/{minimal,serial,kcount}`, ExitBootServices → a bare-metal kernel over COM1.
- `host/os/paging`, x86-64 page-table entries. `host/os/scheduler`, round-robin.
- `host/os/elf64`, an ELF64 header parser (loader). `host/os/bump-alloc`, an allocator.

**Phase 4, drivers**
- `uefi/drivers/uart16550`, a 16550 UART driver (baud/FIFO/tx). `uefi/drivers/pit-timer`, an 8254 PIT.
- `host/os/virtio-desc`, a virtio virtqueue descriptor (virtio-net groundwork).
- `uefi/fs/fat16-dirent`, a FAT16 directory-entry parser.

**Phase 5, the network stack**
- `uefi/net/netframe`, Ethernet + ARP frame construction (ethertype 0x0806).
- `uefi/net/ipv4-checksum`, IPv4 header + Internet checksum (RFC 791).
- `uefi/net/icmp-echo`, ICMP echo reply (RFC 792). `uefi/net/tcp-segment`, TCP header + handshake state machine (RFC 793).
- `uefi/net/dhcp-discover`, DHCP DISCOVER (RFC 2131). `uefi/net/dns-query`, a DNS query (RFC 1035).

**Phase 6, cryptography (simple primitives; the SHA family is beyond the weak model)**
- `host/crypto/rc4`, RC4 keystream (235 159 119). `host/crypto/{crc32,crc16,fnv1a,djb2}` and `uefi/crypto/adler32`, bit-exact checksums/hashes.
- `uefi/crypto/base64`, `host/crypto/base64-decode`, `host/tools/hex-encode`, encodings.
- `uefi/crypto/xorshift`, a PRNG over a mutable global.

## Tools & misc
- `uefi/tools/hexdump`, `uefi/tools/utf8-decode`, `host/tools/{luhn,array-stats}`.
- `freestanding/boot/init`, `host/arithmetic/answer`, the earliest bring-up.

See [`../docs/validation-qwen36-https-stack.md`](../docs/validation-qwen36-https-stack.md)
for the experiment's results, and the compiler in [`../compiler`](../compiler).
