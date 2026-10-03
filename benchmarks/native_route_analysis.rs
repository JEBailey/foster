use foster_compiler::{compile, native};
use std::{env, fs, path::Path};
fn main() {
    let output = env::args().nth(1).expect("output directory");
    fs::create_dir_all(&output).unwrap();
    fs::write(format!("{output}/homes.jsonl"), "").unwrap();
    for source in env::args().skip(2) {
        let name = Path::new(&source).file_stem().unwrap().to_str().unwrap();
        let text = fs::read_to_string(&source).unwrap();
        let compilation = compile(&text).unwrap();
        let route = "ssa";
        let options = native::CompileOptions::default();
        let prepared = native::prepare_with_options(&compilation, options).unwrap();
        fs::write(format!("{output}/{name}-{route}.ir"), prepared.emit_ir()).unwrap();
        fs::write(
            format!("{output}/{name}-{route}.obj"),
            prepared.compile_object(options).unwrap().bytes,
        )
        .unwrap();
        for function in prepared.functions() {
            let ir = function.ir();
            let homes = ir
                .values
                .iter()
                .enumerate()
                .filter_map(|(i, _)| ir.values.hint(i))
                .collect::<std::collections::BTreeSet<_>>();
            let record = format!(
                "{{\"source\":\"{name}\",\"route\":\"{route}\",\"function\":\"{}\",\"homes\":{},\"block_parameters\":{}}}\n",
                ir.name,
                homes.len(),
                ir.blocks.iter().map(|b| b.parameters.len()).sum::<usize>()
            );
            use std::io::Write;
            fs::OpenOptions::new()
                .append(true)
                .open(format!("{output}/homes.jsonl"))
                .unwrap()
                .write_all(record.as_bytes())
                .unwrap();
        }
        println!("{name} {route}: {} functions", prepared.functions().len());
    }
}
