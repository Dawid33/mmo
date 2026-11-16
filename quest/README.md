# quest

The quest script language: compiler (`quest-lang`), bytecode VM (`quest-vm`), in-memory stub host and
runner (`quest-stub`) and the `quest` command (`quest-cli`). Its own Cargo workspace; use the recipes
`just quest-check | quest-test | quest-run <dir> | quest-wasm` from the repository root.

## Commands

```
quest check <dir>
quest run <dir> [--fuel N] [--ticks N] [--mem BYTES] [--trace] [--latency N]
                [--fail KIND[@NTH][:OP]] [--chest KIND=N] [--save FILE] [--load FILE]
```

`<dir>` is read recursively for `*.qs` files (paths relative to it, `/` separators).

`quest check` runs every compiler stage, including layout and code generation, and discards the program: a
passing check means the program compiles. It prints only diagnostics, to stderr; warnings do not fail it.

`quest run` compiles, then runs the program against the stub host: the initialisers, `main` once, then
ticks until nothing is left to do. Script output goes to stdout; diagnostics, reports and trace lines go to
stderr.

| Flag | Meaning | Default |
|---|---|---|
| `--fuel N` | fuel per tick | 2000 |
| `--ticks N` | tick limit | 100000 |
| `--mem BYTES` | VM memory | 262144 |
| `--latency N` | ticks between a `world.*` / `chest.take` request and its answer | 0 |
| `--trace` | one stderr line per tick: `tick=<n> fuel=<used> hash=<16 hex> status=<idle\|busy\|failed>` | off |
| `--fail KIND[@NTH][:OP]` | the NTH (default 1) request of OP (default any) fails; KIND is `timeout`, `lost`, `changed` or `namespace-gone`; repeatable | none |
| `--chest KIND=N` | N items of KIND in the chest; repeatable | empty |
| `--save FILE` | write the VM state after the run | |
| `--load FILE` | start from a saved state | |

`--load` needs the same program the state was saved with; a state saved by another program is refused
(exit 2, the load error). A loaded state that already ran `main` runs it again.
(Migration to a changed program and `--reset` are not available yet.)

A report prints as `error: handler <name> aborted: <fault or error text>` followed by one
`  at <file>:<line>:<col>` per location, or as `warning: event <name> queued` /
`error: event <name> dropped (<reason>)`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | finished (or `check` passed) |
| 1 | compile errors, or no `fn main()` |
| 2 | a task aborted, or the instance failed, or a state could not be loaded |
| 3 | the tick limit was reached |
| 64 | usage error |
| 66 | unreadable directory or file |
