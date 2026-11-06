# Water prototype results (Task 12)

**Date:** 2026-11-06 · **Commit measured:** `ea98f71` (`prototypes/water/`, Tasks 1–11 of
`docs/superpowers/plans/2026-09-22-water-prototype.md`) · **Status:** measured; nothing in the
engine changed.

## Verdict

**Gate: PASS, except (c) by a hair.** `big_drain` bodies runs at a mean 20.2 µs/step against a
20 µs threshold, on a CPU sampled at 1.25–1.3 GHz.

On every comparison that matters, bodies beats the per-cell CA by a wide margin:

| Scenario | Faster per step | Faster to settle |
|---|---|---|
| `big_drain` | 40× | ≥ 96× (the cell CA did not settle at all) |
| `small_spill` | 79× | 198× |
| `dam_breach` | 30× | 50× |

Undo bytes per step are 7–31× smaller. The dense per-cell CA is what the old synthesis named
as the fallback, and these numbers argue against it.

Two findings change the engine design:
- **Brick-journal undo is worse than per-cell undo here, not better.** It costs 4–17× the
  bytes and 10–20% more time. See "Undo shape".
- **Worst-step cost, not the mean, is the risk for rollback.** `big_drain` bodies has a mean of
  20 µs, but a p99 of 258 µs, a max of 1.5 ms, and a max undo of 579 KB in one step. Those
  steps are body restructures. See "Spikes".

## Setup

- **Machine:** i7-12700H laptop, a single-threaded release build.
- **Toolchain:** the repo's pinned nightly.
- **How it ran:** the binary was built under the `cap` prefix and then run with no nice and no
  pinning.
- **Repeats:** each scenario ran 5 times. The timing columns are medians, and the final hash
  was asserted identical across repeats.
- **Power:** AC online, plus both USB-C PD sources online. `scaling_cur_freq` was 1.25 GHz
  before the run and 1.32 GHz between runs. CLAUDE.md warns this setup can clamp cores under
  load. **Compare ratios, not absolute µs.**
- **World:** 64³ cells of 1 m, `FULL = 4096` units per cell.
- **Constants:** see `prototypes/water/src/lib.rs`: `MAX_FLOW` 1024, `FILM` 256,
  `GATE_CAP` 1024, `PUMP_RATE` 512, `SEAM_CAP` 512, `SEAM_LAG_STEPS` 1, `SEAM_GAIN_DIV` 2,
  `EVAP_BELOW` 585, `EVAP_AFTER_STEPS` 64, `TIMING_REPEATS` 5.
- **Commands:**
  - `just water-proto` (per-cell undo);
  - `just water-proto --undo brick`;
  - `just water-proto --seam`;
  - `just water-proto --evap --scenario film`;
  - `just water-proto-wasm` (determinism).

## Tables (raw)

### Per-cell undo journal
```
# evap=off repeats=5 (medians of per-repeat timings)
scenario     mode    steps asleep   mean_us    p99_us    max_us    total_us  visit/st  undoB/st  undoBmax  denseKiB sparseKiB bods  seed  absb  expn split merge  A_sprd   flood relev  drops members     wet   U_left  U_right
settled      cell      100      -      1.38      2.23      2.23         138       0.0         5         5     512.0      75.0    0     0     0     0     0     0       0       0     0      0       0   12800        -        -
settled      bodies    100      -      1.38      2.08      2.08         138       0.0         5         5     512.0      12.8    1     0     0     0     0     0       0       0     0      0   12800       0        -        -
small_spill  cell     1115   1115    260.76    393.53    423.29      290743    1120.6     18250     27548     512.0      75.1    0     0     0     0     0     0       0       0     0      0       0   12812        -        -
small_spill  bodies    443    443      3.31      4.34      5.86        1466      11.5       587       655     512.0      12.9    1     0    12     0     0     0       0       0     0      0   12812       0        -        -
big_drain    cell    60000      -    797.12    940.95   1133.17    47827063    1989.3     59103     68660     512.0     100.1    0     0     0     0     0     0    6284       0     0      0       0   17092        -        -
big_drain    bodies  24597  24597     20.16    257.95   1515.70      495823      57.5      2475    578882     512.0      15.3    1     1   573    30     0     1       0   98324     1      6   16900       0        -        -
bucket       cell       36     36      4.18      7.44      7.44         150      20.0       386       917     512.0       0.2    0     0     0     0     0     0       0       0     0      0       0      27        -        -
bucket       bodies     27     27      3.96      8.11      8.11         107      17.3       464       917     512.0       0.4    1     1     0     2     0     0       0      27     0      0      27       0        -        -
film         cell       14     14      6.74     14.20     14.20          94      27.4       472      1170     512.0       0.2    0     0     0     0     0     0       0       0     0      0       0      41        -        -
film         bodies     14     14      5.69      9.60      9.60          80      27.4       472      1170     512.0       0.2    0     0     0     0     0     0       0       0     0      0       0      41        -        -
u_tube       cell     3256   3256      9.21     16.55     22.70       29973      69.1      1352      2694     512.0       0.6    0     0     0     0     0     0       0       0     0      0       0     104    36992    24557
u_tube       bodies    141    141     11.80     19.36     21.62        1664     115.5      1890      3880     512.0       0.6    1     1    39    42     0     0       0    1001    39      0     104       0    30720    30720
dam_breach   cell     6946   6946    245.33    317.38    376.15     1704029     679.6     20505     27439     512.0      10.5    0     0     0     0     0     0       0       0     0      0       0    1792        -        -
dam_breach   bodies   4076   4076      8.30    107.13    421.37       33835      27.7      2048    151706     512.0       5.7    2     1   377   378     0     0       0   78501   375      5    1794       0        -        -
gate_flood   cell     1192   1192    140.58    228.43    239.35      167570     454.1     11945     19031     512.0      11.2    0     0     0     0     0     0       0       0     0      0       0    1920        -        -
gate_flood   bodies   1192   1192     94.34    141.33    145.01      112448     316.2      8172     12015     512.0       4.5    1     0     0     0     0     0       0       0     0      0    1536     384        -        -
pumped_cistern cell      198    198     15.23     23.02     23.69        3015      69.1      1783      2728     512.0       1.9    0     0     0     0     0     0       0       0     0      0       0     330        -        -
pumped_cistern bodies     83     83      3.21      6.28      6.28         267      10.7       555       806     512.0       1.2    2     1     1     0     0     0       0       8     0      0     329       0        -        -
scoop        cell      165    165     40.30     63.80     65.07        6649     201.1      3397      5171     512.0       9.0    0     0     0     0     0     0       0       0     0      0       0    1536        -        -
scoop        bodies      1      1      1.35      1.35      1.35           1       0.0       323       323     512.0       2.3    1     0     0     0     0     0       0       0     0      0    1536       0        -        -
```

### Brick undo journal
```
# evap=off repeats=5 (medians of per-repeat timings)
scenario     mode    steps asleep   mean_us    p99_us    max_us    total_us  visit/st  undoB/st  undoBmax  denseKiB sparseKiB bods  seed  absb  expn split merge  A_sprd   flood relev  drops members     wet   U_left  U_right
settled      cell      100      -      2.55      2.85      2.85         255       0.0         5         5     512.0      75.0    0     0     0     0     0     0       0       0     0      0       0   12800        -        -
settled      bodies    100      -      1.24      2.27      2.27         124       0.0         5         5     512.0      12.8    1     0     0     0     0     0       0       0     0      0   12800       0        -        -
small_spill  cell     1115   1115    279.64    406.64    415.99      311796    1120.6    144939    186773     512.0      75.1    0     0     0     0     0     0       0       0     0      0       0   12812        -        -
small_spill  bodies    443    443      4.52      5.43      7.35        2003      11.5     10261     10706     512.0      12.9    1     0    12     0     0     0       0       0     0      0   12812       0        -        -
big_drain    cell    60000      -    849.26    996.01   1129.71    50955552    1989.3    254657    300909     512.0     100.1    0     0     0     0     0     0    6284       0     0      0       0   17092        -        -
big_drain    bodies  24597  24597     23.48    280.72   1089.42      577626      57.5     20224    394919     512.0      15.3    1     1   573    30     0     1       0   98324     1      6   16900       0        -        -
bucket       cell       36     36      6.77     10.66     10.66         244      20.0     21050     31142     512.0       0.2    0     0     0     0     0     0       0       0     0      0       0      27        -        -
bucket       bodies     27     27      6.03     10.23     10.23         163      17.3     19103     31142     512.0       0.4    1     1     0     2     0     0       0      27     0      0      27       0        -        -
film         cell       14     14      6.73     12.10     12.10          94      27.4      7047     10381     512.0       0.2    0     0     0     0     0     0       0       0     0      0       0      41        -        -
film         bodies     14     14      6.38     10.45     10.45          89      27.4      7047     10381     512.0       0.2    0     0     0     0     0     0       0       0     0      0       0      41        -        -
u_tube       cell     3256   3256     13.19     22.17     27.89       42931      69.1     25824     41518     512.0       0.6    0     0     0     0     0     0       0       0     0      0       0     104    36992    24557
u_tube       bodies    141    141     15.83     25.57     27.11        2232     115.5     29541     47022     512.0       0.6    1     1    39    42     0     0       0    1001    39      0     104       0    30720    30720
dam_breach   cell     6946   6946    265.76    342.73    368.68     1845953     679.6    109711    140081     512.0      10.5    0     0     0     0     0     0       0       0     0      0       0    1792        -        -
dam_breach   bodies   4076   4076      9.54    118.25    397.03       38875      27.7      9797    114935     512.0       5.7    2     1   377   378     0     0       0   78501   375      5    1794       0        -        -
gate_flood   cell     1192   1192    155.51    251.95    259.40      185363     454.1     81007    108953     512.0      11.2    0     0     0     0     0     0       0       0     0      0       0    1920        -        -
gate_flood   bodies   1192   1192    103.32    151.89    156.92      123158     316.2     56106     62261     512.0       4.5    1     0     0     0     0     0       0       0     0      0    1536     384        -        -
pumped_cistern cell      198    198     19.99     30.18     31.67        3958      69.1     29902     46713     512.0       1.9    0     0     0     0     0     0       0       0     0      0       0     330        -        -
pumped_cistern bodies     83     83      5.95      8.57      8.57         494      10.7     19529     21066     512.0       1.2    2     1     1     0     0     0       0       8     0      0     329       0        -        -
scoop        cell      165    165     46.50     71.35     72.52        7672     201.1     41226     46697     512.0       9.0    0     0     0     0     0     0       0       0     0      0       0    1536        -        -
scoop        bodies      1      1      1.44      1.44      1.44           1       0.0       323       323     512.0       2.3    1     0     0     0     0     0       0       0     0      0    1536       0        -        -
```

### Seam pair
```
# SEAM_GAIN_DIV=2 SEAM_CAP=512 SEAM_LAG_STEPS=1
scenario      steps settled    A_mass   A_level    B_mass   B_level  lvl_diff   sent_AB   sent_BA   bounced  bnc_shr  flips  mass
seam_drain      902    true       887     49153  11009161     40958      8195  11112096         0    102935    0.009      0    ok
seam_lake       225    true   5505415     61440   5504633     61439         1   5698074         0    193441    0.034      0    ok
seam_bounce      11    true  11005952     73718      4096     57344     16374      4096         0         0    0.000      0    ok
```

### Film with evaporation on
```
# evap=on repeats=5 (medians of per-repeat timings)
scenario     mode    steps asleep   mean_us    p99_us    max_us    total_us  visit/st  undoB/st  undoBmax  denseKiB sparseKiB bods  seed  absb  expn split merge  A_sprd   flood relev  drops members     wet   U_left  U_right
film         cell       82     82      2.42     11.83     11.83         198       6.8       113      1170     512.0       0.0    0     0     0     0     0     0       0       0     0      0       0       0        -        -
film         bodies     82     82      2.23      9.72      9.72         183       6.8       113      1170     512.0       0.0    0     0     0     0     0     0       0       0     0      0       0       0        -        -
```

Column notes:
- `visit/st` counts CA cells only. Body floods (seed, expansion, split) are in `flood`. For
  bodies, the honest per-step cell work is `visit/st + flood/steps`.
- `undoB/st` is the mean of the plan's packed-log byte model per step.
- `bnc_shr` is bounced ÷ gross seam flow.

## Ratios: bodies vs per-cell CA (per-cell undo journal)

| Scenario | Mean µs/step (cell → bodies) | Total µs to settle | Cell work per step (visits + flood) | Steps to sleep | Undo B/step |
|---|---|---|---|---|---|
| small_spill | 260.8 → 3.31 (79×) | 290,743 → 1,466 (198×) | 1121 → 11.5 (97×) | 1115 → 443 | 18,250 → 587 (31×) |
| big_drain | 797 → 20.2 (40×) | 47.8 M (not settled at 60k) → 0.50 M (≥96×) | 1989 → 61.5 (32×) | >60k → 24,597 | 59,103 → 2,475 (24×) |
| dam_breach | 245 → 8.30 (30×) | 1.70 M → 33,835 (50×) | 680 → 46.9 (14×) | 6946 → 4076 | 20,505 → 2,048 (10×) |
| gate_flood | 141 → 94.3 (1.5×) | 167,570 → 112,448 (1.5×) | 454 → 316 | 1192 → 1192 (gate-limited) | 11,945 → 8,172 |
| pumped_cistern | 15.2 → 3.21 | 3,015 → 267 (11×) | 69 → 10.8 | 198 → 83 | 1,783 → 555 |
| scoop | 40.3 → 1.35 | 6,649 → 1 | 201 → 0 | 165 → 1 | 3,397 → 323 |
| u_tube | 9.21 → 11.8 (bodies **slower** per step) | 29,973 → 1,664 (18×) | 69 → 122 | 3256 → 141 | 1,352 → 1,890 |
| bucket / film | ≈ equal (tiny) | 150 → 107 / 94 → 80 | — | 36 → 27 / 14 → 14 | ≈ equal |

What the ratios say:
- **Bodies win most where water stands.** A lake draining, a spill into a pool, or a scoop
  turns into one number changing instead of a surface relaxing.
- **Bodies win least where water is in transit.**
  - `gate_flood` streams a fixed flow into an empty basin. Both models spend their time on the
    same falling, spreading CA water, so bodies is only 1.5× faster.
  - `u_tube` costs bodies more per step while it re-checks layers (39 relevels, 1001 flood
    cells), but it settles 23× sooner.

## Correctness gates

| Gate | Result | Evidence |
|---|---|---|
| Mass exact (`accounted() == initial + poured`) | PASS | Every table run exits 0. The CLI exits 1 on any `mass_ok=false`, every scenario × mode × journal, 5 repeats each. The seam table shows `mass ok` for all 3 pairs. |
| Undo exact | PASS | `t5_rollback` (per-cell) and `t8_journal` (brick, from step 0, both modes, at least one multi-brick step), plus t7/t9/t10 undo round trips, in `just water-proto-test` (62 passed, 1 slow ignored). The final reviewer also checked undo then replay on every step 0..250 in both journal modes. |
| Rerun identical | PASS | The final hash is asserted equal across the 5 timing repeats of every scenario. |
| wasm32 == native | PASS | `just water-proto-wasm` at `ea98f71`: all 23 golden lines identical (10 scenarios × 2 modes + 3 seam pairs), "golden hashes match (native == wasm32)". |
| Invariants | PASS | `body::check_invariants` runs in the scenario tests. |

## Behaviour

- **Lake A's surface while draining.**
  - `big_drain` max spread: bodies **0**, cell **6284** units (about 1.5 blocks of slope
    toward the outlet).
  - `dam_breach` also drains flat in bodies mode.
- **U-tube.** The bodies shafts end level: 30720 / 30720 (7.5 F, as expected). The cell CA ends
  at 36992 vs 24557: with no pressure, the far shaft stays 3 blocks lower.
- **Film.**
  - With evaporation off, 41 wet cells remain as a permanent puddle.
  - With it on, all of it evaporates into the ledger by step 82 and 0 wet cells remain. Mass is
    exact through the ledger.
- **Spill symmetry.** The checkerboard plus order-free exchange leaves an imbalance of 39 (x)
  and 156 (z) units out of 16,384, against 2,099 with the original ascending sweep
  (`t6_order`).
- **Seam.**
  - `seam_lake` levels across the seam to within 1 unit in 225 steps.
  - The net flux never reversed (0 flips at gain `diff/2`; `diff/4` also showed 0 flips, but it
    settled less level and slower, as recorded in Task 10).
  - Bounced volume as a share of gross seam flow: **0.9 %** (drain) and **3.4 %** (lake). All
    of it is receiver-side: the face cell filled from its own side during the 1-step lag.
  - `seam_drain` leaves an 887-unit residue in A, inside the ruled bound of 896 (tight).
  - Bounce ping-pong is not proven to terminate in general. Every fixture settles.
- **Residue micro-bodies.** A body draining through a side outlet keeps up to about
  `top_area` units, for example 258 in `dam_breach` A. It is a puddle, never lost water.

## Interface findings (CA ↔ body)

- **No member/CA flicker.** Absorbs stay far below the final member counts: `big_drain`
  573 absorbs vs 16,900 members, `dam_breach` 377 vs 1,794, `u_tube` 39 vs 104.
- **Seeds and merges are rare:**
  - `big_drain`: 1 seed, 1 merge;
  - `dam_breach`: 1 seed, 0 merges.
- **Relevels** (the U-tube rule): 375 in `dam_breach`, 39 in `u_tube`, 1 in `big_drain`. They
  fire once per below-top absorb of a cell with open ground above, so cost tracks how
  tunnel-shaped the container is.
- **Splits:** none in these fixtures (`split` is 0 everywhere). Split under drain is still
  exercised only by the unit test `undo_across_split`, not by a timed scenario.

## Spikes: the rollback risk

- The mean hides the expensive steps. `big_drain` bodies:
  - mean 20 µs, p99 258 µs, max 1.5 ms per step;
  - undo bytes: mean 2.5 KB, max **579 KB** in one step.
- `dam_breach` bodies: max 421 µs and 152 KB.
- Those are the steps that restructure a body: absorbing a wide layer, relevelling, dropping a
  layer. Each one floods the body's layer, and before-images a `Body` whose dense per-layer
  `area` vector (`4 × dims.y` bytes) and member data get copied.
- Under rollback, a mispredict that replays such a step pays it again. Per
  `02-engine-today.md` §2.2, that is up to about 20 × L times a second at a busy seam.
- **Engine implications:**
  1. Body restructures need a **cell budget per step** (the expansion budget exists, but
     relevel and drop do not have one).
  2. A body's undo must be a **delta of its membership spans**, not a whole-body before-image.
- Neither is a reason to drop bodies. Both are design requirements for the engine version.

## Undo shape: brick journal vs per-cell entries

| Scenario (bodies) | Per-cell B/step | Brick B/step | Per-cell µs/step | Brick µs/step |
|---|---|---|---|---|
| small_spill | 587 | 10,261 (17×) | 3.31 | 4.52 |
| big_drain | 2,475 | 20,224 (8×) | 20.2 | 23.5 |
| dam_breach | 2,048 | 9,797 (5×) | 8.30 | 9.54 |
| gate_flood | 8,172 | 56,106 (7×) | 94.3 | 103.3 |

- **Here the brick journal loses.** A per-cell entry in this prototype is 7–9 bytes, and a
  brick image is 5,188 bytes. A step that touches 1–3 cells in each of several bricks pays the
  full brick for each.
- **That does not settle the engine question.** The engine's per-mutation entry is far heavier
  (`02-engine-today.md` §2.1): a value clone, a global mutex, and an enum as large as a whole
  `Client`. Its cost is per entry, not per byte.
- **What the data supports:**
  - Keep water's undo **per changed cell, in its own compact log**, not in the engine's generic
    `Delta` enum.
  - Use the brick image only where a step touches most of a brick (a dense flood front).
  - A hybrid, choosing per brick by how many cells were touched in the step, is the obvious next
    experiment.

## Per-cell baseline caveat

- The per-cell CA here uses the order-free exchange with a midpoint cap (Task 6 rulings). That
  made it settle about 2–4× slower than the original Gauss-Seidel sweep (`small_spill` 313 →
  1115 steps; `big_drain` 32k → about 75k).
- So the bodies ÷ cell *steps-to-settle* ratios above are flattered by up to that factor.
- The per-step cost ratios (µs/step, visits/step) are not affected, and they alone show bodies
  30–80× cheaper on standing water.

## Not modelled (carry into the engine plan)

- Chisel/half-block capacity.
- The finite world sea ledger beyond films: no sea body, no map-edge in/outflow.
- Currents for swimming; rendering.
- A seam with a **parked** neighbour (the pending-volume ledger at the manager).
- Client prediction across a seam (the engine's relay/orphan behaviour, `02-engine-today.md`
  §3).
- Splits during a timed drain.
- Per-step budgets on body restructures, and span-delta body undo (see Spikes).

## Raw files

The tables above are the verbatim CLI output. The runs were made by this session; the scratch
copies are not kept.
