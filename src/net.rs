//! Link dialing: address-family fallback inside one WebSocket endpoint.
//!
//! Racing the two [`WHATSAPP_WEB_WS_URLS`] ports is endpoint racing, and it
//! cannot stand in for address racing. Both URLs name `web.whatsapp.com`, and
//! the pinned `tokio-websockets` 0.13.3 resolver calls
//! `lookup_host(..).next()`: each factory dials exactly one resolved
//! `SocketAddr`. When DNS answers IPv6 first and that route is broken, both
//! racers retry the same dead address and the link waits for a timeout that
//! the port race was never going to shorten.
//!
//! So the address race lives one level down, in [`FallbackTransportFactory`].
//! It resolves the host itself, orders the answers by family (RFC 8305
//! Happy Eyeballs: interleave IPv6 and IPv4), and dials them as a schedule
//! rather than a batch: the first attempt starts at once, the next starts one
//! `ATTEMPT_DELAY` later or as soon as a slot frees up, and at most
//! `MAX_IN_FLIGHT` attempts exist at a time. The first connection wins and
//! aborts the losers.
//!
//! The attempts are spawned tasks on purpose. Pushing a future into a
//! `FuturesUnordered` only enqueues it, so an earlier version that filled the
//! set and slept between the pushes had not dialled anything by the time the
//! last delay expired.
//!
//! One `DIAL_BUDGET` covers the whole dial: resolution, every stagger, TCP,
//! TLS, and the upgrade. An inner deadline may shorten a stage, but the stages
//! draw from that one budget instead of each getting a fresh one, so a peer
//! that accepts TCP and then goes quiet fails on schedule rather than waiting
//! for the link watchdog.
//!
//! Everything above TCP stays the pinned library's: TLS is
//! [`Connector::wrap`] against the ORIGINAL hostname, so SNI and certificate
//! validation are unchanged, and the WebSocket upgrade is
//! [`ClientBuilder::connect_on`], which keeps the URL, its query parameters,
//! and the `Origin` header exactly as the pinned factory sends them. The
//! protocol and the reconnect lifecycle are untouched; this only chooses
//! which address the socket opens and when the attempt is over.
//!
//! The tests drive the production path with a scripted [`Dialer`] and a
//! paused clock, so they assert when each attempt starts rather than that the
//! dial finished inside a generous bound, and against loopback listeners for
//! the real upgrade.
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio_websockets::{ClientBuilder, MaybeTlsStream};
use whatsapp_rust::TokioRuntime;
use whatsapp_rust::transport::{
    Connector, RacingTransportFactory, Transport, TransportEvent, TransportFactory,
    WHATSAPP_WEB_WS_URLS, from_websocket,
};

/// The `Origin` the pinned factory sends by default. Its constant is not
/// re-exported through `whatsapp_rust::transport`, so it is repeated here and
/// pinned by a test: a silent drift would change the upgrade request.
const WHATSAPP_WEB_ORIGIN: &str = "https://web.whatsapp.com";

/// Delay before the next address is tried, RFC 8305's Connection Attempt
/// Delay. Long enough that a fast family is not duplicated, short enough
/// that a broken one is skipped long before its own connect timeout.
const ATTEMPT_DELAY: Duration = Duration::from_millis(250);

/// Total budget for one dial attempt, from the first byte of the resolution
/// to a ready transport.
///
/// It covers DNS, every stagger delay, TCP, TLS, and the WebSocket upgrade.
/// One `timeout` wraps all of `dial`, so a stage that consumes its share
/// cannot hand a fresh budget to the next one, and the link watchdog is not
/// what finally has to notice a black-holed connection.
const DIAL_BUDGET: Duration = Duration::from_secs(10);

/// Resolves a host to every address, for tests that must control ordering
/// and simulate a family that never answers. Production uses the system
/// resolver through [`SystemResolver`].
///
/// The future is boxed by hand rather than written as an `async fn`, so the
/// trait stays object safe and the factory can hold a `dyn AddressResolver`.
pub trait AddressResolver: Send + Sync {
    /// All addresses for `host`, in the order the resolver returns them.
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<Vec<SocketAddr>>> + Send + 'a>,
    >;
}

/// The system resolver: every answer `getaddrinfo` gives, not just the first.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemResolver;

impl AddressResolver for SystemResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<Vec<SocketAddr>>> + Send + 'a>,
    > {
        Box::pin(async move {
            // `tokio::net::lookup_host` yields the full list; the pinned
            // library's resolver throws all but the first away, which is the
            // bug this module exists to fix.
            Ok(tokio::net::lookup_host((host, port)).await?.collect())
        })
    }
}

/// Opens one TCP connection, injectable so the scheduler can be driven by a
/// controlled clock and a scripted dialer in tests. Production uses
/// [`SystemDialer`], which is `TcpStream::connect` and nothing else.
pub trait Dialer: Send + Sync {
    /// Connects to one address. Returning an error releases the attempt slot
    /// so the scheduler can start the next address without waiting.
    fn connect<'a>(
        &'a self,
        address: SocketAddr,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<TcpStream>> + Send + 'a>>;
}

/// The production dialer: the system TCP connect.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemDialer;

impl Dialer for SystemDialer {
    fn connect<'a>(
        &'a self,
        address: SocketAddr,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<TcpStream>> + Send + 'a>>
    {
        Box::pin(TcpStream::connect(address))
    }
}

/// Ceiling on addresses in flight at once.
///
/// Happy Eyeballs only needs a couple of candidates; a host that resolves to
/// a long list must not turn into an unbounded burst of sockets.
const MAX_IN_FLIGHT: usize = 2;

/// One WebSocket URL, dialled across all of the host's addresses.
///
/// Same job as the pinned `TokioWebSocketTransportFactory`, with the address
/// chosen by this module instead of by `lookup_host(..).next()`. The TLS
/// connector, the scheme handling, and the WebSocket upgrade are the pinned
/// library's, so certificate validation, SNI, session resumption, and the
/// upgrade request are unchanged.
pub struct FallbackTransportFactory {
    url: String,
    origin: Option<String>,
    resolver: Arc<dyn AddressResolver>,
    dialer: Arc<dyn Dialer>,
    /// Total budget for one dial, from the start of resolution to a ready
    /// transport. Resolution, the stagger delays, TLS, and the upgrade all
    /// draw from this one budget; an inner deadline may shorten a stage but
    /// never extends or restarts it.
    budget: Duration,
    /// Kept across dials so TLS session resumption survives a reconnect,
    /// exactly as the pinned factory does with its own connector.
    connector: Option<Connector>,
}

impl FallbackTransportFactory {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            origin: Some(WHATSAPP_WEB_ORIGIN.to_string()),
            resolver: Arc::new(SystemResolver),
            dialer: Arc::new(SystemDialer),
            budget: DIAL_BUDGET,
            connector: None,
        }
    }

    /// Overrides DNS. Tests only.
    pub fn with_resolver(mut self, resolver: Arc<dyn AddressResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Overrides how a single TCP connection is opened. Tests only.
    pub fn with_dialer(mut self, dialer: Arc<dyn Dialer>) -> Self {
        self.dialer = dialer;
        self
    }

    /// Overrides the total dial budget. Tests only.
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// Sends a different `Origin`, mirroring the pinned factory's option.
    pub fn with_origin(mut self, origin: impl Into<String>) -> Self {
        self.origin = Some(origin.into());
        self
    }

    /// Sends no `Origin` at all, mirroring the pinned factory's option.
    pub fn without_origin(mut self) -> Self {
        self.origin = None;
        self
    }

    /// Uses a custom TLS connector, mirroring the pinned factory's option.
    pub fn with_connector(mut self, connector: Connector) -> Self {
        self.connector = Some(connector);
        self
    }

    /// Orders addresses the way Happy Eyeballs does: keep the resolver's
    /// order within a family, but alternate families so a dead IPv6 route
    /// cannot own the first several slots. A host with only one family comes
    /// back in its original order, untouched.
    fn interleave_by_family(addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
        let (mut v6, mut v4): (Vec<_>, Vec<_>) =
            addresses.into_iter().partition(|address| address.is_ipv6());
        let mut ordered = Vec::with_capacity(v6.len() + v4.len());
        loop {
            match (v6.is_empty(), v4.is_empty()) {
                (true, true) => break,
                (true, false) => ordered.append(&mut v4),
                (false, true) => ordered.append(&mut v6),
                (false, false) => {
                    ordered.push(v6.remove(0));
                    ordered.push(v4.remove(0));
                }
            }
        }
        ordered
    }

    /// Opens the first address that connects, as a staggered schedule.
    ///
    /// The first attempt starts immediately. The next one starts
    /// `ATTEMPT_DELAY` later, or as soon as a slot frees up if the running
    /// attempts have already failed, and never while
    /// `MAX_IN_FLIGHT` attempts are in flight. The first success returns
    /// at once and [`JoinSet::abort_all`] cancels the losers, closing their
    /// sockets.
    ///
    /// Each attempt is a spawned task, which is what makes it actually run:
    /// pushing a future into a `FuturesUnordered` only enqueues it, and the
    /// sleep that used to separate the pushes meant nothing was dialled until
    /// every delay had already elapsed.
    async fn connect_first_reachable(
        &self,
        addresses: &[SocketAddr],
    ) -> std::io::Result<TcpStream> {
        let mut attempts: tokio::task::JoinSet<(usize, std::io::Result<TcpStream>)> =
            tokio::task::JoinSet::new();
        let mut next = 0usize;
        let mut last_error = None;

        loop {
            // Fill every free slot before waiting on anything. The very first
            // pass starts the first address with no delay at all.
            while next < addresses.len() && attempts.len() < MAX_IN_FLIGHT {
                let index = next;
                next += 1;
                let dialer = Arc::clone(&self.dialer);
                let address = addresses[index];
                attempts.spawn(async move { (index, dialer.connect(address).await) });
            }
            if attempts.is_empty() {
                // Every address was tried and every one failed.
                return Err(last_error.unwrap_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "host resolved to nothing")
                }));
            }

            // A free slot is only worth waiting `ATTEMPT_DELAY` for while
            // there is still an address to try; otherwise wait for the running
            // attempts to settle. The two cases cannot be merged, because an
            // unconditional sleep is exactly what delayed the first connect
            // before.
            let outcome = if next < addresses.len() {
                tokio::select! {
                    settled = attempts.join_next() => Some(settled),
                    _ = tokio::time::sleep(ATTEMPT_DELAY) => None,
                }
            } else {
                Some(attempts.join_next().await)
            };

            let Some(settled) = outcome else {
                // The stagger elapsed: loop round to start the next address.
                continue;
            };
            let Some(settled) = settled else {
                continue;
            };
            match settled {
                Ok((_index, Ok(stream))) => {
                    // The winner. Losers are cancelled as this set drops.
                    attempts.abort_all();
                    return Ok(stream);
                }
                // A join error is a cancelled or panicked attempt, which is
                // just another way of not connecting.
                Ok((_index, Err(error))) => last_error = Some(error),
                Err(_) => continue,
            }
        }
    }

    /// Full dial: resolve, race the addresses, then hand the socket to the
    /// pinned TLS connector and the pinned WebSocket upgrade. The whole body
    /// runs inside one budget, so no stage can outlive it.
    async fn dial(
        &self,
    ) -> anyhow::Result<(Arc<dyn Transport>, async_channel::Receiver<TransportEvent>)> {
        let uri: http::Uri = self
            .url
            .parse()
            .map_err(|error| anyhow::anyhow!("Failed to parse URL: {error}"))?;
        // The host stays the original name for SNI and for the upgrade's
        // `Host` header; only the socket's destination is chosen here.
        let host = uri
            .host()
            .ok_or_else(|| anyhow::anyhow!("URL has no host"))?
            .trim_start_matches('[')
            .trim_end_matches(']');
        let port = uri
            .port_u16()
            .unwrap_or(if uri.scheme_str() == Some("wss") {
                443
            } else {
                80
            });

        let resolved = self.resolver.resolve(host, port).await?;
        let ordered = Self::interleave_by_family(resolved);
        if ordered.is_empty() {
            return Err(anyhow::anyhow!("{host} resolved to no addresses"));
        }
        let stream = self
            .connect_first_reachable(&ordered)
            .await
            .map_err(|error| anyhow::anyhow!("no address of {host} answered: {error}"))?;

        // Reuse the connector across dials so the resumption store inside it
        // is not thrown away on every reconnect.
        let connector = match &self.connector {
            Some(connector) => connector,
            None => DEFAULT_CONNECTOR.get_or_init(default_connector_owned),
        };

        // TLS (or plain) against the ORIGINAL hostname, then the upgrade.
        // `connect_on` performs the HTTP upgrade over the stream we already
        // have, so the URL, its query parameters, and the Origin header are
        // exactly what the pinned factory would have sent.
        //
        // The scheme decides, exactly as the pinned `connect()` does: `wss`
        // wraps in TLS, `ws` stays plain. `Connector::wrap` always wraps, so
        // the plain case must not go through it.
        let stream: MaybeTlsStream<TcpStream> = match uri.scheme_str() {
            Some("wss") => connector
                .wrap(host, stream)
                .await
                .map_err(|error| anyhow::anyhow!("TLS connect failed: {error}"))?,
            Some("ws") => MaybeTlsStream::Plain(stream),
            _ => return Err(anyhow::anyhow!("Unsupported scheme in {}", self.url)),
        };
        let mut builder = ClientBuilder::from_uri(uri.clone()).connector(connector);
        if let Some(origin) = &self.origin
            && let Ok(value) = http::HeaderValue::from_str(origin)
        {
            builder = builder
                .add_header(http::header::ORIGIN, value)
                .map_err(|error| anyhow::anyhow!("Failed to set Origin header: {error}"))?;
        }
        let (ws, _) = builder
            .connect_on(stream)
            .await
            .map_err(|error| anyhow::anyhow!("WebSocket connect failed: {error}"))?;
        Ok(from_websocket(ws))
    }
}

/// Process-wide default TLS connector, so every dial of every factory shares
/// one resumption store instead of building a fresh config per connection.
static DEFAULT_CONNECTOR: std::sync::OnceLock<Connector> = std::sync::OnceLock::new();

/// The pinned library's default connector (rustls with webpki roots).
fn default_connector_owned() -> Connector {
    whatsapp_rust::transport::default_tls_connector()
}

#[async_trait::async_trait]
impl TransportFactory for FallbackTransportFactory {
    async fn create_transport(
        &self,
    ) -> Result<(Arc<dyn Transport>, async_channel::Receiver<TransportEvent>), anyhow::Error> {
        // The single deadline for the whole dial. Dropping the future on
        // expiry cancels whatever stage was running: a resolver that never
        // answers, a TCP connect that never completes, a peer that accepts and
        // then goes quiet during TLS, and an upgrade that is never answered.
        // An inner deadline may shorten a stage, but it cannot restart this
        // one, so the stages cannot add up to more than the budget.
        tokio::time::timeout(self.budget, self.dial())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "dialing {} ran out of its {:?} budget",
                    self.url,
                    self.budget
                )
            })?
    }
}

/// The production link dialer: one address-racing factory per WebSocket URL
/// entry, with the two ports raced by the library's own
/// [`RacingTransportFactory`].
///
/// The two layers are separate capabilities and both are needed: the outer
/// race picks the endpoint (port), the inner one picks the address family.
/// The URL pair arity is pinned by destructuring: adding or removing an entry
/// fails here, not silently at run time.
pub fn racing_transport_factory() -> RacingTransportFactory {
    let [primary_url, secondary_url] = WHATSAPP_WEB_WS_URLS;
    RacingTransportFactory::new(
        Arc::new(FallbackTransportFactory::new(primary_url)),
        Arc::new(FallbackTransportFactory::new(secondary_url)),
        Arc::new(TokioRuntime),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use whatsapp_rust::transport::{Transport, TransportEvent, TransportFactory};

    /// A resolver returning a fixed list, so a test can put a dead family
    /// first and a live one second.
    struct StubResolver {
        addrs: Vec<SocketAddr>,
    }

    impl AddressResolver for StubResolver {
        fn resolve<'a>(
            &'a self,
            _host: &'a str,
            _port: u16,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<Vec<SocketAddr>>> + Send + 'a>,
        > {
            Box::pin(async move { Ok(self.addrs.clone()) })
        }
    }

    fn loopback(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    fn dead_ipv6() -> SocketAddr {
        // Documentation range: routable-looking, never answers.
        SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            443,
        )
    }

    // -----------------------------------------------------------------
    // Scheduler tests.
    //
    // A paused clock cannot prove the stagger on its own: Tokio
    // auto-advances a paused clock to the next pending timer whenever the
    // runtime has nothing else to do (see "Auto-advance" in
    // `tokio::time`), so the stagger delay would fire at once and the
    // schedule would be invisible. Blocking that auto-advance needs a parked
    // blocking thread and still leaves the test driving wall-clock time.
    //
    // So the schedule is observed directly instead. Each address is
    // scripted, the dialer records the ORDER in which addresses are
    // dialled, and a `Notify` lets the test decide exactly when a scripted
    // attempt answers. Every assertion is about that order and about what
    // got cancelled, never about elapsed time.
    // -----------------------------------------------------------------

    /// What a scripted address does when dialled.
    #[derive(Clone)]
    enum Plan {
        /// Fails as soon as it is dialled.
        Fail,
        /// Never settles unless cancelled.
        Hang,
        /// Answers when the test releases it.
        Release {
            /// A real socket opened during setup, handed over on release.
            /// Shared so dial attempts can clone the plan without moving it.
            stream: Arc<std::sync::Mutex<std::net::TcpStream>>,
            /// Signalled by the test to let this attempt answer.
            gate: Arc<tokio::sync::Notify>,
        },
    }

    /// Records the order in which addresses were dialled, and how each
    /// attempt ended.
    #[derive(Default)]
    struct Log {
        started: Mutex<Vec<SocketAddr>>,
        finished: Mutex<Vec<(SocketAddr, &'static str)>>,
    }

    impl Log {
        /// Addresses in the order they were dialled.
        fn order(&self) -> Vec<SocketAddr> {
            self.started.lock().expect("lock").clone()
        }

        /// How many attempts were cancelled by the scheduler.
        fn cancelled(&self) -> usize {
            self.finished
                .lock()
                .expect("lock")
                .iter()
                .filter(|(_, outcome)| *outcome == "cancelled")
                .count()
        }
    }

    struct Scripted {
        plans: Mutex<Vec<(SocketAddr, Plan)>>,
        log: Arc<Log>,
    }

    impl Dialer for Scripted {
        fn connect<'a>(
            &'a self,
            address: SocketAddr,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<TcpStream>> + Send + 'a>,
        > {
            let plan = self
                .plans
                .lock()
                .expect("lock")
                .iter()
                .find(|(planned, _)| *planned == address)
                .map(|(_, plan)| plan.clone())
                .unwrap_or(Plan::Fail);
            let log = Arc::clone(&self.log);
            Box::pin(async move {
                log.started.lock().expect("lock").push(address);
                match plan {
                    Plan::Fail => {
                        log.finished.lock().expect("lock").push((address, "error"));
                        Err(std::io::Error::new(
                            std::io::ErrorKind::ConnectionRefused,
                            "scripted failure",
                        ))
                    }
                    Plan::Hang => {
                        // Dropping this guard on cancellation is what tells a
                        // cancelled loser apart from one that never ran.
                        struct Marked(Arc<Log>, SocketAddr);
                        impl Drop for Marked {
                            fn drop(&mut self) {
                                self.0
                                    .finished
                                    .lock()
                                    .expect("lock")
                                    .push((self.1, "cancelled"));
                            }
                        }
                        let _marked = Marked(Arc::clone(&log), address);
                        std::future::pending::<()>().await;
                        unreachable!()
                    }
                    Plan::Release { stream, gate } => {
                        // Waits for the test to release this attempt, so the
                        // winner is chosen by the test and not by timing.
                        gate.notified().await;
                        let clone = stream.lock().expect("lock").try_clone()?;
                        log.finished.lock().expect("lock").push((address, "ok"));
                        Ok(TcpStream::from_std(clone)?)
                    }
                }
            })
        }
    }

    /// A scripted factory plus the log and the gates, so a test can release
    /// an attempt by index.
    fn scripted(
        plans: Vec<(SocketAddr, Plan)>,
    ) -> (
        FallbackTransportFactory,
        Arc<Log>,
        Vec<SocketAddr>,
        Vec<Arc<tokio::sync::Notify>>,
    ) {
        let addresses: Vec<SocketAddr> = plans.iter().map(|(address, _)| *address).collect();
        let log = Arc::new(Log::default());
        let gates: Vec<Arc<tokio::sync::Notify>> = plans
            .iter()
            .map(|(_, plan)| match plan {
                Plan::Release { gate, .. } => Arc::clone(gate),
                _ => Arc::default(),
            })
            .collect();
        let factory = FallbackTransportFactory::new("ws://web.whatsapp.test/ws/chat")
            .with_resolver(Arc::new(StubResolver {
                addrs: addresses.clone(),
            }))
            .with_dialer(Arc::new(Scripted {
                plans: Mutex::new(plans),
                log: Arc::clone(&log),
            }));
        (factory, log, addresses, gates)
    }

    /// A loopback WebSocket server that completes the real handshake, so the
    /// production upgrade path is exercised end to end. Records the upgrade
    /// request it received, which is how the tests below prove the URL, its
    /// query parameters, and the `Origin` header survive address racing.
    async fn ws_listener() -> (
        SocketAddr,
        Arc<std::sync::Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binds");
        let addr = listener.local_addr().expect("addr");
        let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let recorded = Arc::clone(&seen);
        let handle = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    let Ok((request, _stream)) =
                        tokio_websockets::ServerBuilder::new().accept(socket).await
                    else {
                        return;
                    };
                    let origin = request
                        .headers()
                        .get(http::header::ORIGIN)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    recorded.lock().expect("lock").push(format!(
                        "{} {}|origin={origin}|host={}",
                        request.method(),
                        request.uri(),
                        request
                            .headers()
                            .get(http::header::HOST)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default(),
                    ));
                });
            }
        });
        (addr, seen, handle)
    }

    #[test]
    fn link_uses_two_distinct_endpoints() {
        assert_eq!(WHATSAPP_WEB_WS_URLS.len(), 2);
        assert_ne!(WHATSAPP_WEB_WS_URLS[0], WHATSAPP_WEB_WS_URLS[1]);
        // Construction only: no network involved.
        let _ = racing_transport_factory();
    }

    #[test]
    fn families_interleave_so_a_dead_ipv6_cannot_own_the_first_slots() {
        let ordered = FallbackTransportFactory::interleave_by_family(vec![
            dead_ipv6(),
            SocketAddr::new(
                IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2)),
                443,
            ),
            loopback(1),
            loopback(2),
        ]);
        // IPv4 is tried second, not fourth: one IPv6 attempt cannot crowd
        // out the working family.
        assert!(ordered[0].is_ipv6());
        assert!(ordered[1].is_ipv4());
        assert!(ordered[2].is_ipv6());
        assert!(ordered[3].is_ipv4());
    }

    #[test]
    fn a_single_family_keeps_the_resolvers_order() {
        let v4 = vec![loopback(1), loopback(2)];
        assert_eq!(
            FallbackTransportFactory::interleave_by_family(v4.clone()),
            v4
        );
        let v6 = vec![dead_ipv6()];
        assert_eq!(
            FallbackTransportFactory::interleave_by_family(v6.clone()),
            v6
        );
    }

    /// The production path with a stuck IPv6 first and a live IPv4 second:
    /// the connection must complete over IPv4 without waiting out the
    /// Opens a real loopback stream for a scripted answer, so the release
    /// path hands over a genuine `TcpStream` without touching the timed path.
    async fn open_listener() -> (
        SocketAddr,
        Arc<std::sync::Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        ws_listener().await
    }

    /// (a) The first address answers at once: the race returns on it and no
    /// other address is ever dialled, so the stagger cost nothing.
    #[tokio::test]
    async fn an_immediate_winner_never_waits_or_starts_another_attempt() {
        let (addr, _seen, server) = open_listener().await;
        let gate = Arc::new(tokio::sync::Notify::new());
        let (factory, log, addresses, _gates) = scripted(vec![
            (
                loopback(1),
                Plan::Release {
                    stream: Arc::new(Mutex::new(
                        tokio::net::TcpStream::connect(addr)
                            .await
                            .expect("setup")
                            .into_std()
                            .unwrap(),
                    )),
                    gate: Arc::clone(&gate),
                },
            ),
            (loopback(2), Plan::Hang),
            (loopback(3), Plan::Hang),
        ]);
        // Release the first address before the race even starts, so it wins
        // without any delay.
        gate.notify_one();
        factory
            .connect_first_reachable(&addresses)
            .await
            .expect("the first address answers");
        assert_eq!(
            log.order(),
            vec![loopback(1)],
            "only the first address should have been dialled"
        );
        assert_eq!(log.cancelled(), 0, "no loser existed to cancel");
        server.abort();
    }

    /// (b) The first address stays pending, so the second one is dialled
    /// and can answer: the stagger is what moves the race forward.
    #[tokio::test]
    async fn a_pending_first_address_lets_the_second_start_and_answer() {
        let (addr, _seen, server) = open_listener().await;
        let gate = Arc::new(tokio::sync::Notify::new());
        let (factory, log, addresses, _gates) = scripted(vec![
            (loopback(1), Plan::Hang),
            (
                loopback(2),
                Plan::Release {
                    stream: Arc::new(Mutex::new(
                        tokio::net::TcpStream::connect(addr)
                            .await
                            .expect("setup")
                            .into_std()
                            .unwrap(),
                    )),
                    gate: Arc::clone(&gate),
                },
            ),
        ]);
        // Release the second address's gate up front, but the second address
        // is not dialled until the first one has had its interval. The race
        // must still reach it.
        gate.notify_one();
        factory
            .connect_first_reachable(&addresses)
            .await
            .expect("the second address answers after the stagger");
        assert_eq!(
            log.order(),
            vec![loopback(1), loopback(2)],
            "the second address must be dialled while the first is pending"
        );
        // The pending first address is cancelled once the second wins.
        assert_eq!(log.cancelled(), 1, "the pending loser must be cancelled");
        server.abort();
    }

    /// (c) A first address that fails at once must not end the race: the
    /// next address is still dialled and gets its chance to answer.
    #[tokio::test]
    async fn a_fast_failure_does_not_abort_the_race() {
        let (addr, _seen, server) = open_listener().await;
        let gate = Arc::new(tokio::sync::Notify::new());
        let (factory, log, addresses, _gates) = scripted(vec![
            (loopback(1), Plan::Fail),
            (
                loopback(2),
                Plan::Release {
                    stream: Arc::new(Mutex::new(
                        tokio::net::TcpStream::connect(addr)
                            .await
                            .expect("setup")
                            .into_std()
                            .unwrap(),
                    )),
                    gate: Arc::clone(&gate),
                },
            ),
        ]);
        gate.notify_one();
        factory
            .connect_first_reachable(&addresses)
            .await
            .expect("the second address answers despite the first failing");
        assert_eq!(
            log.order(),
            vec![loopback(1), loopback(2)],
            "a fast failure must not stop the next address from being dialled"
        );
        server.abort();
    }

    /// (d) With many addresses the in-flight ceiling is respected and every
    /// loser is cancelled once a winner appears.
    #[tokio::test]
    async fn many_addresses_respect_the_ceiling_and_cancel_the_losers() {
        let (addr, _seen, server) = open_listener().await;
        let gate = Arc::new(tokio::sync::Notify::new());
        // Six candidates; only the last one answers.
        let mut plans: Vec<(SocketAddr, Plan)> =
            (1..=6).map(|port| (loopback(port), Plan::Hang)).collect();
        plans[5] = (
            loopback(6),
            Plan::Release {
                stream: Arc::new(Mutex::new(
                    tokio::net::TcpStream::connect(addr)
                        .await
                        .expect("setup")
                        .into_std()
                        .unwrap(),
                )),
                gate: Arc::clone(&gate),
            },
        );
        let (factory, log, addresses, _gates) = scripted(plans);
        gate.notify_one();
        factory
            .connect_first_reachable(&addresses)
            .await
            .expect("the last address answers");
        assert_eq!(
            log.order().len(),
            6,
            "every address should eventually be tried: {:?}",
            log.order()
        );
        // Every hanging loser is cancelled, not left running.
        assert_eq!(
            log.cancelled(),
            5,
            "the five hanging losers must be cancelled: {:?}",
            log.order()
        );
        // The winner is the sixth, so the ceiling was respected on the way.
        assert_eq!(
            *log.order().last().expect("some address was dialled"),
            loopback(6)
        );
        server.abort();
    }

    // -----------------------------------------------------------------
    // Total-budget tests: each stage of a dial must be inside the one
    // budget, not merely the TCP race.
    // -----------------------------------------------------------------

    /// A resolver that never answers, so the budget has to cover DNS.
    struct HangingResolver;

    impl AddressResolver for HangingResolver {
        fn resolve<'a>(
            &'a self,
            _host: &'a str,
            _port: u16,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<Vec<SocketAddr>>> + Send + 'a>,
        > {
            Box::pin(std::future::pending())
        }
    }

    /// A resolver whose own future is dropped when the budget expires, which
    /// is how a DNS lookup gets cancelled.
    #[tokio::test(start_paused = true)]
    async fn a_resolver_that_never_answers_is_cancelled_by_the_budget() {
        let factory = FallbackTransportFactory::new("wss://web.whatsapp.test/ws/chat")
            .with_resolver(Arc::new(HangingResolver))
            .with_budget(Duration::from_secs(4));
        let started_at = tokio::time::Instant::now();
        let error = match factory.create_transport().await {
            Ok(_) => panic!("a resolver that never answers cannot produce a transport"),
            Err(error) => error,
        };
        assert_eq!(
            started_at.elapsed().as_millis() as u64,
            4_000,
            "the budget must expire the resolution on schedule"
        );
        assert!(
            error.to_string().contains("budget"),
            "unexpected error: {error}"
        );
    }

    /// TCP that never connects, with the clock paused so the budget is what
    /// ends the wait.
    #[tokio::test(start_paused = true)]
    async fn a_tcp_attempt_that_never_connects_is_bounded_by_the_budget() {
        let plans: Vec<(SocketAddr, Plan)> =
            (1..=4).map(|port| (loopback(port), Plan::Hang)).collect();
        let (factory, _log, _addresses, _gates) = scripted(plans);
        let started_at = tokio::time::Instant::now();
        let error = match factory
            .with_budget(Duration::from_secs(3))
            .create_transport()
            .await
        {
            Ok(_) => panic!("hanging TCP cannot produce a transport"),
            Err(error) => error,
        };
        assert_eq!(
            started_at.elapsed().as_millis() as u64,
            3_000,
            "the budget must end the wait, not a per-attempt timeout"
        );
        assert!(error.to_string().contains("budget"), "unexpected: {error}");
    }

    /// A server that accepts TCP and then says nothing, so the stall is in
    /// the WebSocket upgrade rather than in the connect.
    #[tokio::test(start_paused = true)]
    async fn a_server_that_never_finishes_the_upgrade_is_bounded_by_the_budget() {
        // Accepts the connection and then stays silent, holding it open.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binds");
        let addr = listener.local_addr().expect("addr");
        let held = tokio::spawn(async move {
            let mut sockets = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                // Read the request but never answer the upgrade.
                sockets.push(socket);
            }
        });
        let factory = FallbackTransportFactory::new(format!(
            "ws://web.whatsapp.test:{}/ws/chat",
            addr.port()
        ))
        .with_resolver(Arc::new(StubResolver { addrs: vec![addr] }))
        .with_budget(Duration::from_secs(2));
        let started_at = tokio::time::Instant::now();
        let error = match factory.create_transport().await {
            Ok(_) => panic!("a silent server cannot complete the upgrade"),
            Err(error) => error,
        };
        assert_eq!(
            started_at.elapsed().as_millis() as u64,
            2_000,
            "an unanswered upgrade must be bounded by the same budget"
        );
        assert!(error.to_string().contains("budget"), "unexpected: {error}");
        held.abort();
    }

    /// A server that accepts and closes without speaking HTTP fails on its
    /// own, and must not be retried into a longer total.
    #[tokio::test(start_paused = true)]
    async fn a_stage_failure_does_not_restart_the_budget() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binds");
        let addr = listener.local_addr().expect("addr");
        let breaker = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                drop(socket);
            }
        });
        let factory = FallbackTransportFactory::new(format!(
            "ws://web.whatsapp.test:{}/ws/chat",
            addr.port()
        ))
        .with_resolver(Arc::new(StubResolver { addrs: vec![addr] }))
        .with_budget(Duration::from_secs(5));
        let started_at = tokio::time::Instant::now();
        let result = factory.create_transport().await;
        let elapsed = started_at.elapsed().as_millis() as u64;
        breaker.abort();
        // It failed early, and the whole dial still stayed inside its budget.
        assert!(
            result.is_err(),
            "a closed connection cannot complete a dial"
        );
        assert!(
            elapsed < 5_000,
            "the dial took {elapsed} ms, past its own budget"
        );
    }
    /// The production path with a stuck IPv6 first and a live IPv4 second:
    /// the connection must complete over IPv4 without waiting out the
    /// dead family's own connect timeout.
    #[tokio::test]
    async fn a_stuck_ipv6_falls_back_to_ipv4_without_waiting_it_out() {
        let (addr, seen, _server) = ws_listener().await;
        // A query parameter the upgrade must carry through unchanged, standing
        // in for the edge-routing `ED` param the library adds.
        let url = format!(
            "ws://web.whatsapp.test:{}/ws/chat?ED=stale-edge-token",
            addr.port()
        );
        let factory =
            FallbackTransportFactory::new(url.clone()).with_resolver(Arc::new(StubResolver {
                // IPv6 first, exactly as a dual-stack resolver orders it.
                addrs: vec![dead_ipv6(), addr],
            }));
        let started = std::time::Instant::now();
        let (_transport, _events) =
            tokio::time::timeout(Duration::from_secs(8), factory.create_transport())
                .await
                .expect("the IPv4 address answers well before the deadline")
                .expect("upgrade succeeds over IPv4");
        // One stagger interval, not the race deadline and not an OS-level
        // connect timeout for the dead route.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}, which means it waited on the dead family",
            started.elapsed()
        );
        // Choosing the address must not rewrite the request: the path and the
        // query parameter survive the race intact.
        let requests = seen.lock().expect("lock").clone();
        assert_eq!(requests.len(), 1, "exactly one upgrade reached the server");
        assert!(
            requests[0].contains("/ws/chat?ED=stale-edge-token"),
            "the URL and its query parameter changed: {}",
            requests[0]
        );
        // The Origin the pinned factory sends by default is still sent, and
        // the peer is still named by the original hostname, not the address.
        assert!(
            requests[0].contains(&format!("origin={WHATSAPP_WEB_ORIGIN}")),
            "the Origin header changed: {}",
            requests[0]
        );
        assert!(
            requests[0].contains("web.whatsapp.test"),
            "the Host must stay the original hostname: {}",
            requests[0]
        );
    }

    /// Same integration path, but IPv6 is the only family that works.
    #[tokio::test]
    async fn a_working_ipv6_is_used_when_it_is_the_only_address() {
        // Bind an IPv6 loopback listener when the host supports it; skip
        // cleanly otherwise, since the point is the ordering, not IPv6 itself.
        let Ok(listener) = tokio::net::TcpListener::bind("[::1]:0").await else {
            eprintln!("skipping: no IPv6 loopback on this host");
            return;
        };
        let addr = listener.local_addr().expect("addr");
        let handle = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let _ = tokio_websockets::ServerBuilder::new().accept(socket).await;
                });
            }
        });
        let factory = FallbackTransportFactory::new(format!("ws://[::1]:{}/ws/chat", addr.port()))
            .with_resolver(Arc::new(StubResolver { addrs: vec![addr] }));
        let result = tokio::time::timeout(Duration::from_secs(8), factory.create_transport()).await;
        handle.abort();
        let (_transport, _events) = result
            .expect("no global timeout")
            .expect("IPv6 upgrade succeeds");
    }

    /// Every address refused: the dial fails in bounded time, and the error
    /// names the deadline rather than hanging.
    #[tokio::test]
    async fn when_every_address_fails_the_dial_fails_in_bounded_time() {
        // Two ports nothing listens on, so the connect is refused at once.
        let dead_a = loopback(9);
        let dead_b = loopback(10);
        let factory = FallbackTransportFactory::new("ws://web.whatsapp.test/ws/chat")
            .with_resolver(Arc::new(StubResolver {
                addrs: vec![dead_a, dead_b],
            }));
        let started = std::time::Instant::now();
        let result =
            tokio::time::timeout(Duration::from_secs(30), factory.create_transport()).await;
        assert!(
            result.is_ok(),
            "the dial must not hang past its own deadline"
        );
        let error = match result.expect("no global timeout") {
            Ok(_) => panic!("no address answered, so the dial must fail"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("no address of web.whatsapp.test answered"),
            "unexpected error: {error}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "took {:?}",
            started.elapsed()
        );
    }

    /// A host that resolves to nothing fails immediately instead of racing
    /// an empty list.
    #[tokio::test]
    async fn an_empty_resolution_fails_immediately() {
        let factory = FallbackTransportFactory::new("ws://web.whatsapp.test/ws/chat")
            .with_resolver(Arc::new(StubResolver { addrs: vec![] }));
        let error = match factory.create_transport().await {
            Ok(_) => panic!("nothing resolved, so the dial must fail"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("no addresses"),
            "unexpected error: {error}"
        );
    }

    /// Reconnection resolves again: a factory that succeeded against one
    /// address picks up a changed answer on the next dial.
    #[tokio::test]
    async fn each_dial_resolves_again() {
        let (first, _a, _s1) = ws_listener().await;
        let (second, _b, _s2) = ws_listener().await;
        let answers = Arc::new(std::sync::Mutex::new(vec![first]));
        struct Rotating {
            answers: Arc<std::sync::Mutex<Vec<SocketAddr>>>,
            served: Arc<AtomicUsize>,
        }
        impl AddressResolver for Rotating {
            fn resolve<'a>(
                &'a self,
                _host: &'a str,
                _port: u16,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = std::io::Result<Vec<SocketAddr>>> + Send + 'a>,
            > {
                Box::pin(async move {
                    self.served.fetch_add(1, Ordering::SeqCst);
                    let mut guard = self.answers.lock().expect("lock");
                    Ok(vec![guard.remove(0)])
                })
            }
        }
        {
            let mut guard = answers.lock().expect("lock");
            guard.push(second);
        }
        let served = Arc::new(AtomicUsize::new(0));
        let factory = FallbackTransportFactory::new(format!(
            "ws://web.whatsapp.test:{}/ws/chat",
            first.port()
        ))
        .with_resolver(Arc::new(Rotating {
            answers,
            served: Arc::clone(&served),
        }));
        factory.create_transport().await.expect("first dial");
        factory
            .create_transport()
            .await
            .expect("second dial re-resolves");
        assert_eq!(served.load(Ordering::SeqCst), 2, "DNS runs on every dial");
    }

    struct FakeTransport {
        disconnects: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Transport for FakeTransport {
        async fn send(&self, _data: bytes::Bytes) -> Result<(), anyhow::Error> {
            Ok(())
        }

        async fn disconnect(&self) {
            self.disconnects.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A controllable endpoint: hangs, fails, or answers after a delay,
    /// counting starts and recording when its future is dropped.
    struct FakeFactory {
        mode: Mode,
        started: Arc<AtomicUsize>,
        dropped: Arc<AtomicUsize>,
        disconnects: Arc<AtomicUsize>,
    }

    #[derive(Clone, Copy)]
    enum Mode {
        Hang,
        Fail,
        SucceedAfter(std::time::Duration),
    }

    struct DropFlag(Arc<AtomicUsize>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl TransportFactory for FakeFactory {
        async fn create_transport(
            &self,
        ) -> Result<(Arc<dyn Transport>, async_channel::Receiver<TransportEvent>), anyhow::Error>
        {
            self.started.fetch_add(1, Ordering::SeqCst);
            let _guard = DropFlag(Arc::clone(&self.dropped));
            match self.mode {
                Mode::Hang => futures::future::pending().await,
                Mode::Fail => anyhow::bail!("endpoint down"),
                Mode::SucceedAfter(delay) => {
                    tokio::time::sleep(delay).await;
                    let (_tx, rx) = async_channel::bounded(1);
                    Ok((
                        Arc::new(FakeTransport {
                            disconnects: Arc::clone(&self.disconnects),
                        }),
                        rx,
                    ))
                }
            }
        }
    }

    fn fakes(
        mode: Mode,
    ) -> (
        FakeFactory,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    ) {
        let started = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let disconnects = Arc::new(AtomicUsize::new(0));
        (
            FakeFactory {
                mode,
                started: Arc::clone(&started),
                dropped: Arc::clone(&dropped),
                disconnects: Arc::clone(&disconnects),
            },
            started,
            dropped,
            disconnects,
        )
    }

    fn runtime() -> Arc<dyn whatsapp_rust::Runtime> {
        Arc::new(TokioRuntime)
    }

    /// The port race still behaves as before: these factories stand in for
    /// whole endpoints, so they prove endpoint racing, not address racing.
    #[tokio::test]
    async fn a_hanging_endpoint_loses_to_a_working_one() {
        let (primary, primary_started, primary_dropped, _) = fakes(Mode::Hang);
        let (secondary, _, _, _) = fakes(Mode::SucceedAfter(std::time::Duration::from_millis(10)));
        let racing = RacingTransportFactory::new(Arc::new(primary), Arc::new(secondary), runtime());
        let won = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            racing.create_transport(),
        )
        .await
        .expect("global timeout")
        .expect("secondary wins");
        let _ = won;
        assert_eq!(primary_started.load(Ordering::SeqCst), 1);
        assert_eq!(
            primary_dropped.load(Ordering::SeqCst),
            1,
            "loser dial aborted"
        );
    }

    #[tokio::test]
    async fn a_lone_endpoint_failure_waits_for_the_other() {
        let (primary, _, _, _) = fakes(Mode::Fail);
        let (secondary, _, _, _) = fakes(Mode::SucceedAfter(std::time::Duration::from_millis(50)));
        let racing = RacingTransportFactory::new(Arc::new(primary), Arc::new(secondary), runtime());
        racing
            .create_transport()
            .await
            .expect("secondary still wins");
    }

    #[tokio::test]
    async fn both_endpoints_failing_surfaces_an_error() {
        let (primary, _, _, _) = fakes(Mode::Fail);
        let (secondary, _, _, _) = fakes(Mode::Fail);
        let racing = RacingTransportFactory::new(Arc::new(primary), Arc::new(secondary), runtime());
        assert!(racing.create_transport().await.is_err());
    }

    #[tokio::test]
    async fn an_already_open_loser_is_disconnected_in_background() {
        let disconnects = Arc::new(AtomicUsize::new(0));
        let endpoint = || FakeFactory {
            mode: Mode::SucceedAfter(std::time::Duration::from_millis(0)),
            started: Arc::default(),
            dropped: Arc::default(),
            disconnects: Arc::clone(&disconnects),
        };
        let racing =
            RacingTransportFactory::new(Arc::new(endpoint()), Arc::new(endpoint()), runtime());
        racing
            .create_transport()
            .await
            .expect("winner returns at once");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while disconnects.load(Ordering::SeqCst) == 0 {
            assert!(std::time::Instant::now() < deadline, "loser must be closed");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}
