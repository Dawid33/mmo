//! `worldctl`: world-store maintenance (`world_store::ctl`). `just worldctl …`.

fn main() {
    let _ = simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default());
    let args: Vec<String> = std::env::args().skip(1).collect();
    match world_store::ctl::run(&args) {
        Ok(said) => println!("{said}"),
        Err(e) => {
            eprintln!("worldctl: {e}");
            std::process::exit(1);
        }
    }
}
