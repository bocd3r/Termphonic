mod actions;
mod app;
mod audio;
mod models;
mod runtime;
mod search;
mod session;
mod ui;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    app::run().await
}
