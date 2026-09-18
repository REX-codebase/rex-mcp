// REX Harness - frontend-only shell.
// No backend commands are registered yet. The UI currently renders from a
// local sample-data engine and labels it as such. When the agent core lands,
// it will live behind invoke handlers here.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running REX Harness");
}
