# quest

The quest script language: compiler (`quest-lang`), bytecode VM (`quest-vm`), in-memory stub host and
runner (`quest-stub`) and the `quest` command (`quest-cli`). Its own Cargo workspace; use the recipes
`just quest-check | quest-test | quest-run <dir> | quest-wasm` from the repository root.

## Commands

```
quest check <dir>
quest run <dir> [--fuel N] [--ticks N] [--mem BYTES] [--trace] [--latency N]
                [--fail KIND[@NTH][:OP]] [--chest KIND=N] [--save FILE] [--load FILE] [--reset]
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
| `--save FILE` | write the program and its VM state after the run | |
| `--load FILE` | start from a saved state | |
| `--reset` | re-initialise the instance (after `--load`): as if the script had just been placed | off |

A state file holds the program it was saved with. `--load` of a state saved by the same program resumes it
exactly; a loaded state that already ran `main` runs it again. A state saved by a changed script is
**migrated** (spec §6.9): each top-level variable whose name and type still fit keeps its value, a new one
runs its initialiser, and a value whose variable is gone or no longer fits is kept aside as an orphan (a
later edit that brings the name back with a fitting type takes it again); queued events go to the new
handlers, parked handlers are cancelled. The migration prints to stderr:

```
carried: runs
initialised: bonus
orphaned: gold
orphan: gold: int = Int(7)
```

— `carried`, `adopted`, `initialised` and `orphaned` list names (a line only when its list is not empty),
then one `orphan:` line per value kept aside (older orphans too), then `events dropped: N` and
`handlers cancelled: N` when not 0. A file that cannot be loaded exits 2. A migration can be refused (a
value too large to read out, say): the run prints the reason and exits 2 without writing a state file, so
the old state stays valid for the old script.

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
