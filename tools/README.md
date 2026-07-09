# tools/

## sandbox.sh --- hard resource limits (REQUIRED for untrusted code)

Runs any command with RAM/CPU/time/process limits via cgroup v2
(`systemd-run --user`). **Firm rule (JMV): ALL compilation and ALL execution
---especially programs written by qwen3.6 during validation--- run through
this sandbox**, to avoid risking the machine against OOM, fork-bombs, or
infinite loops.

```bash
tools/sandbox.sh [--mem 2G] [--cpu 200%] [--time 30] [--tasks 256] -- <cmd...>

# compiler tests (heavy compilation):
tools/sandbox.sh --mem 4G --cpu 400% --time 300 -- cargo test

# execute a generated Sond binary (untrusted code -> strict limits):
tools/sandbox.sh --mem 256M --cpu 100% --time 5 --tasks 32 -- ./my_program
```

Tested: kills a RAM devourer (exit 137/OOM), cuts by time, lets normal pass.
