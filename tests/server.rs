// Copied from https://github.com/seanmonstar/reqwest/blob/ab49de875ec2326abf25f52f54b249a28e43b69c/tests/support/server.rs
// with some slight tweaks
use std::convert::Infallible;
use std::future::Future;
use std::net;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use hyper::service::{make_service_fn, service_fn};
use tokio::runtime;
use tokio::sync::oneshot;

pub struct Server {
    addr: net::SocketAddr,
    successful_hits: Arc<Mutex<u8>>,
    total_hits: Arc<Mutex<u8>>,
    // The panic_rx channel is to make sure the test fails if the server panics.
    // If the server panics, the message is not sent and the recv call panics.
    panic_rx: std_mpsc::Receiver<()>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl Server {
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr.to_string())
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr.to_string(), path)
    }

    pub fn host(&self) -> String {
        String::from("127.0.0.1")
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn assert_hits(&self, hits: u8) {
        assert_eq!(*self.successful_hits.lock().unwrap(), hits);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }

        if !::std::thread::panicking() {
            let total_hits = *self.total_hits.lock().unwrap();
            let successful_hits = *self.successful_hits.lock().unwrap();
            let failed_hits = total_hits - successful_hits;
            assert!(total_hits > 0, "test server exited without being called");
            assert!(
                failed_hits == 0,
                "numbers of panicked requests: {}",
                failed_hits
            );
            self.panic_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("test server should not panic");
        }
    }
}

pub fn http<F, Fut>(func: F) -> Server
where
    F: Fn(hyper::Request<hyper::Body>) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = hyper::Response<hyper::Body>> + Send + 'static,
{
    //Spawn new runtime in thread to prevent reactor execution context conflict
    thread::spawn(move || {
        let successful_hits = Arc::new(Mutex::new(0));
        let total_hits = Arc::new(Mutex::new(0));
        let rt = runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("new rt");
        let srv = {
            let successful_hits = successful_hits.clone();
            let total_hits = total_hits.clone();
            #[allow(clippy::async_yields_async)]
            rt.block_on(async move {
                let make_service = make_service_fn(move |_| {
                    let func = func.clone();
                    let successful_hits = successful_hits.clone();
                    let total_hits = total_hits.clone();
                    async move {
                        Ok::<_, Infallible>(service_fn(move |req| {
                            let fut = func(req);
                            let successful_hits = successful_hits.clone();
                            let total_hits = total_hits.clone();
                            async move {
                                *total_hits.lock().unwrap() += 1;
                                let res = fut.await;
                                *successful_hits.lock().unwrap() += 1;
                                Ok::<_, Infallible>(res)
                            }
                        }))
                    }
                });
                // Port 0 is used to obtain a dynamically assigned port.
                // See https://networkengineering.stackexchange.com/a/64784
                hyper::Server::bind(&([127, 0, 0, 1], 0).into()).serve(make_service)
            })
        };

        let addr = srv.local_addr();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let srv = srv.with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });

        let (panic_tx, panic_rx) = std_mpsc::channel();
        thread::Builder::new()
            .name("test-server".into())
            .spawn(move || {
                rt.block_on(srv).unwrap();
                let _ = panic_tx.send(());
            })
            .expect("thread spawn");

        Server {
            addr,
            successful_hits,
            total_hits,
            panic_rx,
            shutdown_tx: Some(shutdown_tx),
        }
    })
    .join()
    .unwrap()
}
