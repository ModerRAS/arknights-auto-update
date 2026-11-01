#[tokio::main]
async fn main() {
    if let Err(err) = ark_updater::run().await {
        eprintln!("启动失败：{err:#}");
        std::process::exit(1);
    }
}
