# sudo perf script record -F997 --call-graph dwarf,16384 -e cpu-clock ./target/x86_64-unknown-linux-gnu/debug/fern
# `profiling` cargo profile = release + debug info; dev's opt-level 1
# distorts hot loops ~2.5x and doesn't represent production costs.
cargo build --bin client --profile profiling
sudo env VOXEL_SKIP_MENU=1 perf script record -F9997 --call-graph dwarf,16384 -e cpu-clock ./target/profiling/client
# sudo perf script record -F997 --call-graph dwarf,16384 -e cpu-clock /root/.cargo/bin/cargo build
sudo chmod +r perf.data
hotspot
