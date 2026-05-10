mod commands;
mod runtime_client;

#[tokio::main]
async fn main() {
    if let Err(err) = commands::run(std::env::args().skip(1).collect()).await {
        println!("{}", serde_json::to_string_pretty(&err).unwrap_or_else(|_| "{\"ok\":false}".to_string()));
        std::process::exit(1);
    }
}
