use std::path::Path;
use std::{env, fs};

use toml::Value;

fn main() {
    let toml_content =
        fs::read_to_string("../versions.toml").expect("Failed to read versions.toml");

    let config: Value = toml::from_str(&toml_content).expect("Failed to parse TOML");

    let mut rust_code = String::from("// Auto-generated code from versions.toml\n\n");

    match config {
        Value::Table(table) => {
            for (key, val) in table {
                match val {
                    Value::String(s) => {
                        let const_value = format!("\"{}\"", s);
                        rust_code.push_str(&format!(
                            "pub const {}: &str = {};\n",
                            key.to_uppercase(),
                            const_value
                        ));
                    }
                    _ => {
                        panic!("Invalid value type in versions.toml: {:?}", val);
                    }
                };
            }
        }
        _ => {
            panic!("Invalid value type in versions.toml: {:?}", config);
        }
    }

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("versions.rs");
    fs::write(&dest_path, rust_code).expect("Failed to write generated code");

    // Tell Cargo to rerun this script if the TOML file changes
    println!("cargo:rerun-if-changed=../versions.toml");
}
