//! Main-thread job marshalling (docs/0068 D9): a parsed request becomes a
//! boxed closure that runs on the foreground executor with `&mut App`
//! access and reports back over its own reply channel. No gpui type ever
//! crosses a thread boundary — the closures only carry plain protocol
//! data.

use futures::channel::mpsc::UnboundedSender;
use gpui::App;
use serde_json::Value;

use crate::ops::OpsRegistry;
use crate::protocol::{Request, RpcError, error_code};

/// A unit of main-thread work. The closure sends its own reply (unbounded,
/// never blocks) so the executor stays completely generic.
pub type Job = Box<dyn FnOnce(&mut App) + Send>;

/// Queue the main-thread work for one request and block the connection
/// thread for its reply. Op resolution happens *on the main thread* —
/// the registry is a main-thread global and providers may want `App`
/// access for module state (D9); an unknown op therefore never leaves
/// the main thread's answer path.
pub(crate) fn dispatch(job_tx: &UnboundedSender<Job>, request: Request) -> Result<Value, RpcError> {
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let Request {
        id: _,
        v: _,
        op,
        params,
    } = request;
    job_tx
        .unbounded_send(Box::new(move |app: &mut App| {
            let outcome = match app.try_global::<OpsRegistry>() {
                None => Err(RpcError::new(error_code::INTERNAL, "ops registry missing")),
                Some(registry) => match registry.resolve(&op) {
                    Some(provider) => provider.handle(app, &op, &params),
                    None => Err(RpcError::new(
                        error_code::UNKNOWN_OP,
                        format!("no op named {op:?}"),
                    )),
                },
            };
            let _ = reply_tx.send(outcome);
        }))
        .map_err(|_| RpcError::new(error_code::INTERNAL, "agent bridge job queue is gone"))?;
    reply_rx
        .recv()
        .map_err(|_| RpcError::new(error_code::INTERNAL, "main-thread executor dropped the reply"))?
}
