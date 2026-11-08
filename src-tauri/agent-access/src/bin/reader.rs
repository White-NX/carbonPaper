#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    let result = (|| -> Result<(), String> {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let configuration =
            serde_json::from_slice(&bytes).map_err(|_| "AGENT_INVALID_CONFIGURATION")?;
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?
            .block_on(carbonpaper_agent_access::transport::serve(configuration))
    })();
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
