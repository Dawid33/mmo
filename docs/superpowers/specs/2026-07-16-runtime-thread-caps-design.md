# Runtime Thread Caps Design

## Goal

Prevent the native client and dedicated server from consuming every logical CPU by default. Both runtimes use at most four worker threads unless an operator explicitly selects another positive limit.

## Client

The native Bevy application configures `TaskPoolPlugin` with `max_total_threads = 4` by default. `VOXEL_CLIENT_WORKER_THREADS` remains the client override and applies to the combined Bevy task pools rather than independently granting four threads to each pool.

The existing async-compute assignment continues to receive half of the available Bevy worker budget for terrain meshing. Its maximum is bounded by the same total cap so assignment policy cannot accidentally defeat the process-wide Bevy limit.

The wasm client does not read native process environment configuration and retains Bevy's platform behavior.

## Server

The dedicated server configures its Tokio multi-thread runtime explicitly with four workers by default. `TOKIO_WORKER_THREADS` remains the server override, matching Tokio's conventional environment variable and the existing render-benchmark recipe.

This cap covers the dedicated server's Tokio networking workers only. Existing region, LOD, and world-management OS threads and the client-side networking runtime are unchanged.

## Configuration and Failure Behavior

Both defaults are four workers. Environment overrides must be positive integers. Invalid values fail immediately at startup with a message naming the offending variable and expected format.

The Bevy override continues to require at least four workers because Bevy has three task pools and the renderer needs async-compute headroom. Tokio accepts any positive worker count.

## Testing

Focused unit tests prove:

- the client default produces a four-thread total Bevy cap;
- a valid client override replaces that default;
- invalid and undersized client values are rejected;
- the Tokio default is four workers;
- a valid Tokio override replaces that default;
- zero and malformed Tokio values are rejected.

Implementation follows test-driven development: add focused failing tests, observe the expected failures, make the minimum runtime changes, then run formatting, focused suites, and `just test-full`.

## Non-Goals

- Changing simulation-region, world-generation, LOD, game-loop, or tick-timer thread topology.
- Dynamically resizing pools after startup.
- Adding CLI flags or a shared configuration framework.
- Changing render-benchmark defaults beyond inheriting the same validated runtime configuration.
