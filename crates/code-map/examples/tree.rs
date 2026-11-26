fn main() {
    let path = std::env::args().nth(1).expect("a file to parse");
    let text = std::fs::read_to_string(&path).expect("a readable file");
    println!("{}", code_map::tree(&path, &text));
    let reading = code_map::read(&path, text.as_bytes());
    let symbols: Vec<_> = reading
        .symbols
        .iter()
        .map(|symbol| format!("{} {}", symbol.kind, symbol.name))
        .collect();
    println!(
        "symbols {symbols:?}\nscope {:?}\nimports {:?}\nmentions {:?}",
        reading.scope, reading.imports, reading.mentions
    );
}
