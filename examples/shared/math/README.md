# Sond shared libraries (`.so`), importable from C, C++, Go, Rust, Python

Sond compiles to a C-ABI shared object. Its functions already use the System V AMD64
calling convention, so a `pub fn` is a plain C-callable symbol, no glue, no wrappers.

```sh
sondc math.snd  --shared -o libsondmath.so
sondc types.snd --shared -o libsondtypes.so
nm -D --defined-only libsondmath.so     # the exported symbols (pub fn only)
```

- `pub fn f(...)` → exported symbol `f`. `priv fn` is hidden (internal only).
- Methods `impl T { fn m(...) }` export as `T_m`.
- Name it `libNAME.so` if you want link-time `-lNAME`; any name works for dlopen/ctypes.

## What crosses the boundary (verified)

| Sond | C type | How it is passed |
|------|--------|------------------|
| `i64` (all Sond ints) | `int64_t` | value, in a register, trivial |
| `struct T { ... }` | `struct { int64_t ...; }` (one `int64_t` per field, in order) | **by pointer**, Sond receives a struct as its base address |
| `*T` (pointer struct) | `T*` | pointer; mutate the caller's struct to "return" one |
| C string | `char*` (take an `i64`, read with `peek8`; return `cstr("...")`) | pointer to NUL-terminated bytes |
| float / double |, | **not yet**: Sond has no float type (would need SSE/xmm) |

Returning a struct = fill a caller-owned one through a `*T` out-pointer (the robust
C-ABI pattern), e.g. `pub fn point_translate(p: *Point, dx: i64, dy: i64)`.

## Import snippets (all verified to print the right numbers)

**Python (ctypes)**, always set `restype`/`argtypes` (`c_int64`), or a 64-bit return truncates:
```python
import ctypes
lib = ctypes.CDLL("./libsondmath.so")
lib.sond_add.restype = ctypes.c_int64
lib.sond_add.argtypes = [ctypes.c_int64, ctypes.c_int64]
print(lib.sond_add(2, 3))            # 5
class Rect(ctypes.Structure): _fields_ = [("w", ctypes.c_int64), ("h", ctypes.c_int64)]
lib.rect_area.argtypes = [ctypes.POINTER(Rect)]; lib.rect_area.restype = ctypes.c_int64
print(lib.rect_area(ctypes.byref(Rect(4, 5))))   # 20
```

**C / C++**, one dual header (`sondmath.h`) with `extern "C"` guards serves both:
```c
#include "sondmath.h"
sond_add(2, 3);                      /* 5 */
```
```sh
cc  main.c   -L. -lsondmath -Wl,-rpath,'$ORIGIN' -o app     # link-time
c++ main.cpp -L. -lsondmath -Wl,-rpath,'$ORIGIN' -o app     # C++: extern "C" via header
# runtime instead:  void* h = dlopen("./libsondmath.so", RTLD_NOW); dlsym(h, "sond_add");
```

**Rust**, `extern "C"`, link with `-l sondmath` (or the `libloading` crate at runtime):
```rust
extern "C" { fn sond_add(a: i64, b: i64) -> i64; }
fn main() { println!("{}", unsafe { sond_add(2, 3) }); }   // 5
```
```sh
rustc main.rs -L. -l sondmath -C link-arg=-Wl,-rpath,'$ORIGIN' -o app
```

**Go**, cgo (below) or, recommended for a pure C `.so`, `github.com/ebitengine/purego`:
```go
/*
#cgo LDFLAGS: -L. -lsondmath -Wl,-rpath,${SRCDIR}
#include <stdint.h>
int64_t sond_add(int64_t a, int64_t b);
*/
import "C"
func main() { println(int64(C.sond_add(2, 3))) }   // 5
```

Inspect any Sond `.so` with `readelf -h` (Type: DYN), `nm -D --defined-only`,
`readelf -d | grep TEXTREL` (should be empty).
