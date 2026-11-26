//! The static loop check of the browser safety gate ([`makepad_story_web_export::shader_loops`])
//! on a shader pack file, such as the one a web trace's pass 1 records
//! (`makepad-web-trace trace --record-pack <file>`).
//!
//! ```text
//! cargo run --release -p makepad-story-web-export --example pack_loops -- <pack>
//! ```
//! Prints the counts and every loop that is not provably bounded; exit status 1 when there is one.
use makepad_story_web_export::shader_loops::check_pack;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: pack_loops <shader pack>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(2);
    });
    match check_pack(&bytes) {
        Ok(loops) => {
            println!("{path}: {} shaders, {} bounded loops (at most {} passes), {} guarded, {} unbounded", loops.shaders, loops.bounded, loops.max_trips, loops.guarded, loops.unbounded.len());
            let entries = makepad_platform::makepad_script::shader_pack::inspect_shader_pack(&bytes);
            for u in &loops.unbounded {
                println!("  {} {} line {}: {} ({})", u.shader, u.stage, u.line, u.head, u.reason);
                // the loop in its source, with what follows (`--context`)
                if std::env::args().any(|a| a == "--context") {
                    let code = (0..entries.len()).map(|i| entries.entry(i).unwrap()).find(|e| e.name() == u.shader).map(|e| if u.stage == "vertex" { e.vertex().to_string() } else { e.fragment().to_string() }).unwrap_or_default();
                    for (n, l) in code.lines().enumerate().skip(u.line as usize - 1).take(12) {
                        println!("      {:5} {}", n + 1, l);
                    }
                }
            }
            if !loops.unbounded.is_empty() {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(2);
        }
    }
}
