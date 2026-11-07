//! `login`: the login service (`login serve`) and its admin commands (see `login::ctl`).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let _ = simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default());
    // `--db <url>` may stand anywhere; `serve` is the default command.
    let route = login::ctl::route(&args).unwrap_or_else(|e| fail(&e));
    if route.serve {
        let config = match login::LoginConfig::from_env() {
            Ok(c) => c,
            Err(e) => fail(&e),
        };
        let config = match route.db {
            Some(db) => login::LoginConfig { db_url: db, ..config },
            None => config,
        };
        let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| fail(&e.to_string()));
        if let Err(e) = rt.block_on(login::serve(config)) {
            fail(&e.to_string());
        }
        return;
    }
    if let Err(e) = login::ctl::run_ctl(&args, &mut std::io::stdout()) {
        fail(&e);
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("login: {msg}");
    std::process::exit(2);
}
