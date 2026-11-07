//! `worldctl`: world-store maintenance (`world_store::ctl`). `just worldctl …`.

fn main() {
    let _ = simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default());
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = world_store::ctl::run(&args) {
        eprintln!("worldctl: {e}");
        std::process::exit(1);
    }
}
