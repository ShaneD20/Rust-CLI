
use std::env;
use std::fs;
use std::process;

fn count(path: &str) -> Result<(usize, usize), std::io::Error> {
    let contents = fs::read_to_string(path)?;
    let lines = contents.lines().count();
    let words = contents.split_whitespace().count();
    Ok((lines, words))
}

fn main() {
    // like argv in C: index 0 is the program name
    let args: Vec<String> = env::args().collect(); 

    // args.get(1) => Option<&String>: Some(&String) or None
    let path = match args.get(1) {
        Some(p) => p,
        None => {
            eprintln!("Usage: {} <path>", args[0]);
            process::exit(2);
        }
    };

    match count(path) {
        Ok((lines, words)) => println!("{lines} lines, {words} words"),
        Err(error) => {
            eprintln!("Error reading {path}: {error}");
            process::exit(1);
        }
    }


    // &str: borrowed string slice (similar std::string_view)

}
