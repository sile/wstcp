use std::net::SocketAddr;
use wstcp::ProxyServer;

fn main() -> noargs::Result<()> {
    env_logger::init();

    let mut args = noargs::raw_args();
    args.metadata_mut().app_name = env!("CARGO_PKG_NAME");
    args.metadata_mut().app_description = env!("CARGO_PKG_DESCRIPTION");

    if noargs::VERSION_FLAG.take(&mut args).is_present() {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    noargs::HELP_FLAG.take_help(&mut args);

    let bind_addr: SocketAddr = noargs::opt("bind-addr")
        .ty("ADDR")
        .default("0.0.0.0:13892")
        .doc("TCP address to which the WebSocket proxy binds")
        .take(&mut args)
        .then(|o| o.value().parse())?;

    let real_server_addr: SocketAddr = noargs::arg("<REAL_SERVER_ADDR>")
        .example("127.0.0.1:3000")
        .doc("The TCP address of the real server")
        .take(&mut args)
        .then(|a| a.value().parse())?;

    if let Some(help) = args.finish()? {
        print!("{help}");
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let proxy = ProxyServer::new(bind_addr, real_server_addr).await?;
        proxy.run().await
    })?;

    Ok(())
}
