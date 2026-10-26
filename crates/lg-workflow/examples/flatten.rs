//! Dev tool: print a workflow (editor, API or blueprint) as an API graph plus its
//! exposed fields and model files. Used to derive manifest templates.
//!
//! `cargo run -p lg-workflow --example flatten -- <workflow.json> [object_info.json]`

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let wf: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
    let info_path = args.get(2).cloned().unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/object_info.json"
        )
        .into()
    });
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(info_path).unwrap()).unwrap();
    let imp = lg_workflow::import::import(&wf, &info).unwrap();
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
        "title": imp.title, "description": imp.description, "exposed": imp.exposed, "models": imp.models, "graph": imp.graph
    })).unwrap());
}
