//! `cargo run -p aaes-core --example outline -- <file>`: print the shaped read for a file.
fn main() {
    let path = std::env::args().nth(1).expect("path");
    let text = std::fs::read_to_string(&path).expect("read");
    let cfg = aaes_core::shape::ShapeConfig::default();
    match aaes_core::shape::shape_read(&path, &text, &cfg) {
        Some(r) => {
            let ctx = aaes_core::shape::read_context(&path, &r, r.total_lines);
            println!("{ctx}");
            eprintln!(
                "raw ~{} tok -> delivered ~{} tok (head {} + context {})",
                r.raw_tokens,
                cfg.est_tokens(r.head.len() + ctx.len()),
                cfg.est_tokens(r.head.len()),
                cfg.est_tokens(ctx.len())
            );
        }
        None => eprintln!("pass-through (below threshold)"),
    }
}
