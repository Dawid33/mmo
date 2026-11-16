//! Helpers shared by the compiler's test binaries.

#![allow(dead_code)]

use quest_lang::{Diagnostic, SourceTree, check};

/// A source tree of `(path, text)` pairs.
pub fn tree(files: &[(&str, &str)]) -> SourceTree {
    SourceTree {
        files: files
            .iter()
            .map(|(path, text)| (path.to_string(), text.to_string()))
            .collect(),
    }
}

/// Every diagnostic the front end built so far reports on `files`, against the stub host's API, in the
/// order `check` gives them.
pub fn diagnostics(files: &[(&str, &str)]) -> Vec<Diagnostic> {
    match check(&tree(files), &quest_stub::stub_api()) {
        Ok(warnings) => warnings,
        Err(all) => all,
    }
}

/// The code, file and line of every diagnostic, sorted.
pub fn diags(files: &[(&str, &str)]) -> Vec<(String, String, u32)> {
    let mut out: Vec<(String, String, u32)> = diagnostics(files)
        .into_iter()
        .map(|d| (d.code.to_string(), d.file, d.line))
        .collect();
    out.sort();
    out
}

/// What a run of a program printed and reported, and the heap it left behind.
pub struct Out {
    pub lines: Vec<String>,
    pub reports: Vec<quest_vm::host::Report>,
    /// Heap words in use once the run is quiescent (zombies reaped), minus the words in use right after
    /// the top-level initialisers ran.
    pub leaked_words: i64,
}

/// Compiles `files` against the stub host's API; panics with the rendered diagnostics on an error.
pub fn compile(files: &[(&str, &str)]) -> quest_vm::Program {
    let t = tree(files);
    match quest_lang::compile(&t, &quest_stub::stub_api()) {
        Ok(c) => c.program,
        Err(diags) => {
            let text: Vec<String> = diags.iter().map(|d| quest_lang::render(d, &t)).collect();
            panic!("compile failed:\n{}", text.join("\n"));
        }
    }
}

/// Runs `main` with the default stub host and 2 000 fuel per tick, for at most 10 000 ticks.
pub fn run(files: &[(&str, &str)]) -> Out {
    run_with(files, quest_stub::StubConfig::default(), 2_000)
}

/// Compiles `files` twice (the bytes must be identical), initialises the instance, then runs `main`
/// under `fuel` per tick until it is quiescent or 10 000 ticks ran.
pub fn run_with(files: &[(&str, &str)], cfg: quest_stub::StubConfig, fuel: u64) -> Out {
    let program = compile(files);
    assert_eq!(
        program.to_bytes(),
        compile(files).to_bytes(),
        "compiling twice gave different bytes"
    );
    let mut runner = quest_stub::Runner::new(
        std::sync::Arc::new(program),
        quest_vm::consts::MEM_BYTES_DEFAULT,
        cfg,
    )
    .expect("the program loads");
    // The initialisers run before anything else; `main` is asked for once they are done.
    for _ in 0..10_000 {
        if runner.vm.status() != quest_vm::Status::Busy {
            break;
        }
        runner.step(fuel);
    }
    let baseline = i64::from(runner.vm.heap_used_words());
    runner.run_main(fuel, 10_000);
    Out {
        lines: runner.host.output.clone(),
        reports: runner.host.reports.clone(),
        leaked_words: i64::from(runner.vm.heap_used_words()) - baseline,
    }
}
