use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Client as AsyncClient;
use reqwest::blocking::Client as BlockingClient;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const POOL_MAX_IDLE: usize = 5;
const TCP_KEEPALIVE: Duration = Duration::from_secs(30);

static BLOCKING_CLIENT: OnceLock<BlockingClient> = OnceLock::new();
static ASYNC_CLIENT: OnceLock<AsyncClient> = OnceLock::new();

pub fn blocking() -> &'static BlockingClient {
    BLOCKING_CLIENT.get_or_init(|| {
        BlockingClient::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .pool_max_idle_per_host(POOL_MAX_IDLE)
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .tcp_keepalive(TCP_KEEPALIVE)
            .tcp_nodelay(true)
            .build()
            .expect("failed to build HTTP client")
    })
}

pub fn async_client() -> &'static AsyncClient {
    ASYNC_CLIENT.get_or_init(|| {
        AsyncClient::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .pool_max_idle_per_host(POOL_MAX_IDLE)
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .http2_keep_alive_interval(TCP_KEEPALIVE)
            .http2_keep_alive_while_idle(true)
            .tcp_keepalive(TCP_KEEPALIVE)
            .tcp_nodelay(true)
            .build()
            .expect("failed to build async HTTP client")
    })
}
