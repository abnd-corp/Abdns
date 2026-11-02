//! Runtime d'entrées-sorties : un seul fil, lancé au premier usage. Il porte le relais (UDP,
//! TCP), le client DoH et l'attente des signaux d'arrêt ; une requête y est une tâche, pas un
//! fil. La garde et le canal de commande, eux, gardent leurs fils et l'utilisent par `block_on`.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use tokio::runtime::{Builder, Handle};

static HANDLE: OnceLock<Handle> = OnceLock::new();

pub fn handle() -> &'static Handle {
    HANDLE.get_or_init(|| {
        let runtime = Builder::new_current_thread().enable_all().build().expect("cannot start the I/O runtime");
        let handle = runtime.handle().clone();
        std::thread::Builder::new()
            .name("abdns-io".into())
            .spawn(move || runtime.block_on(std::future::pending::<()>()))
            .expect("cannot start the I/O thread");
        handle
    })
}

/// Attend un futur depuis un fil hors du runtime (garde, canal de commande, tests).
pub fn block_on<F: Future>(future: F) -> F::Output {
    handle().block_on(future)
}

/// `stop` passe à vrai à la réception de SIGINT, SIGTERM ou SIGHUP (Ctrl+C sous Windows).
pub fn on_stop(stop: &Arc<AtomicBool>) -> Result<()> {
    let _runtime = handle().enter();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        for kind in [SignalKind::interrupt(), SignalKind::terminate(), SignalKind::hangup()] {
            let mut signal = signal(kind)?;
            let stop = stop.clone();
            handle().spawn(async move {
                signal.recv().await;
                stop.store(true, Ordering::SeqCst);
            });
        }
    }
    #[cfg(not(unix))]
    {
        let stop = stop.clone();
        handle().spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                stop.store(true, Ordering::SeqCst);
            }
        });
    }
    Ok(())
}
