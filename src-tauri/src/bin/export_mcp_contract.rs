//! Prints the runtime MCP contract as stable, pretty JSON.

fn main() {
    let contract = carbonpaper_lib::mcp_contract::contract_document();
    println!(
        "{}",
        serde_json::to_string_pretty(&contract).expect("MCP contract must serialize")
    );
}
