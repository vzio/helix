use vortex_loader::grammar::{build_grammars, fetch_grammars};

const STRICT: bool = true;

fn main() {
    if std::env::var("VORTEX_DISABLE_AUTO_GRAMMAR_BUILD").is_err() {
        fetch_grammars(STRICT).expect("Failed to fetch tree-sitter grammars");
        build_grammars(Some(std::env::var("TARGET").unwrap()), STRICT)
            .expect("Failed to compile tree-sitter grammars");
    }
}
