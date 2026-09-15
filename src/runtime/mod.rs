use crate::desktop::Engine;
use crate::request::Request;
use base64::Engine as _;
use rquickjs::{Context, Function, Runtime, Value};
use serde_json::Value as Json;
use std::{
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

#[derive(Default)]
struct Cancellation {
    cancelled: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}
impl Cancellation {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Acquire)
    }
}
struct CancelOnDrop(Arc<Cancellation>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0
            .cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        self.0.notify.notify_one();
    }
}

enum Command {
    Eval {
        code: String,
        timeout: Duration,
        cancellation: Arc<Cancellation>,
        reply: oneshot::Sender<Result<Vec<Json>, String>>,
    },
    Reset {
        reply: oneshot::Sender<Result<(), String>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
pub struct Javascript {
    sender: mpsc::Sender<Command>,
}

impl Javascript {
    pub fn new(engine: Arc<Engine>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            let mut runtime: Option<Session> = None;
            while let Ok(command) = receiver.recv() {
                match command {
                    Command::Shutdown { reply } => {
                        drop(runtime.take());
                        let _ = reply.send(());
                        break;
                    }
                    Command::Reset { reply } => {
                        runtime = None;
                        handle.block_on(engine.reset());
                        let _ = reply.send(Ok(()));
                    }
                    Command::Eval {
                        code,
                        timeout,
                        cancellation,
                        reply,
                    } => {
                        if reply.is_closed() {
                            continue;
                        }
                        if runtime.is_none() {
                            match Session::new(engine.clone(), handle.clone()) {
                                Ok(session) => runtime = Some(session),
                                Err(error) => {
                                    let _ = reply.send(Err(error));
                                    continue;
                                }
                            }
                        }
                        let result = runtime.as_ref().expect("initialized runtime").evaluate(
                            &code,
                            timeout,
                            cancellation.clone(),
                        );
                        if cancellation.is_cancelled()
                            || result.is_err()
                                && runtime.as_ref().is_some_and(|session| {
                                    Instant::now() >= *session.deadline.lock().expect("deadline")
                                })
                        {
                            runtime = None;
                            handle.block_on(engine.reset());
                        }
                        let _ = reply.send(result);
                    }
                }
            }
        });
        Self { sender }
    }

    pub async fn evaluate(&self, code: String, timeout: Duration) -> Result<Vec<Json>, String> {
        if code.len() > 128 * 1024 {
            return Err("Script exceeds 128 KiB".into());
        }
        let (reply, result) = oneshot::channel();
        let cancellation = Arc::new(Cancellation::default());
        let _cancel = CancelOnDrop(cancellation.clone());
        self.sender
            .send(Command::Eval {
                code,
                timeout,
                cancellation,
                reply,
            })
            .map_err(|error| error.to_string())?;
        result.await.map_err(|error| error.to_string())?
    }

    pub async fn reset(&self) -> Result<(), String> {
        let (reply, result) = oneshot::channel();
        self.sender
            .send(Command::Reset { reply })
            .map_err(|error| error.to_string())?;
        result.await.map_err(|error| error.to_string())?
    }
    pub async fn shutdown(&self) {
        let (reply, result) = oneshot::channel();
        if self.sender.send(Command::Shutdown { reply }).is_ok() {
            let _ = result.await;
        }
    }
}

struct Session {
    context: Context,
    _runtime: Runtime,
    deadline: Arc<Mutex<Instant>>,
    output: Arc<Mutex<Output>>,
    cancellation: Arc<Mutex<Arc<Cancellation>>>,
}

#[derive(Default)]
struct Output {
    items: Vec<Json>,
    bytes: usize,
}

impl Session {
    fn new(engine: Arc<Engine>, handle: tokio::runtime::Handle) -> Result<Self, String> {
        let runtime = Runtime::new().map_err(|error| error.to_string())?;
        runtime.set_memory_limit(128 * 1024 * 1024);
        runtime.set_max_stack_size(1024 * 1024);
        let deadline = Arc::new(Mutex::new(Instant::now() + Duration::from_secs(30)));
        let interrupt = deadline.clone();
        let cancellation = Arc::new(Mutex::new(Arc::new(Cancellation::default())));
        let interrupted = cancellation.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || {
            interrupted.lock().expect("cancellation").is_cancelled()
                || Instant::now() >= *interrupt.lock().expect("deadline")
        })));
        let context = Context::full(&runtime).map_err(|error| error.to_string())?;
        let output: Arc<Mutex<Output>> = Arc::new(Mutex::new(Output::default()));
        context.with(|ctx| -> rquickjs::Result<()> {
            let native_deadline=deadline.clone();
            let native_cancellation=cancellation.clone();
            ctx.globals().set("__call",Function::new(ctx.clone(),move |payload:String|->String {
                let result=match serde_json::from_str::<Request>(&payload) {
                    Ok(request)=>{
                        let remaining=native_deadline.lock().expect("deadline").saturating_duration_since(Instant::now());
                        let cancellation=native_cancellation.lock().expect("cancellation").clone();
                        handle.block_on(async {
                            if cancellation.is_cancelled() {return Err(crate::error::fail("cancelled","Request cancelled"));}
                            tokio::select! {
                                _=cancellation.notify.notified()=>Err(crate::error::fail("cancelled","Request cancelled; observe before deciding whether to retry")),
                                result=tokio::time::timeout(remaining,engine.dispatch(request))=>match result {
                                    Ok(result)=>result,
                                    Err(_)=>Err(crate::error::fail("timeout","Operation timed out; observe before deciding whether to retry")),
                                }
                            }
                        })
                    },
                    Err(error)=>Err(crate::error::fail("invalid_argument",error.to_string())),
                };
                match result {
                    Ok(value)=>serde_json::json!({"value":value}).to_string(),
                    Err(error)=>serde_json::json!({"error":error}).to_string(),
                }
            })?)?;
            let emitted=output.clone();
            ctx.globals().set("__emit",Function::new(ctx.clone(),move |payload:String| -> bool {
                if let Ok(value)=serde_json::from_str::<Json>(&payload) {
                    let mut output=emitted.lock().expect("output");
                    if output.items.len()>=64 || output.bytes.saturating_add(payload.len())>16*1024*1024 {return false;}
                    output.bytes+=payload.len();
                    output.items.push(value);
                    return true;
                }
                false
            })?)?;
            ctx.globals().set("__decode",Function::new(ctx.clone(),|payload:String|->Vec<u8> {
                base64::engine::general_purpose::STANDARD.decode(payload).unwrap_or_default()
            })?)?;
            ctx.globals().set("__encode",Function::new(ctx.clone(),|bytes:Vec<u8>|->String {
                base64::engine::general_purpose::STANDARD.encode(bytes)
            })?)?;
            ctx.eval::<(),_>(include_str!("bootstrap.js"))?;
            Ok(())
        }).map_err(|error|error.to_string())?;
        Ok(Self {
            context,
            _runtime: runtime,
            deadline,
            output,
            cancellation,
        })
    }

    fn evaluate(
        &self,
        code: &str,
        timeout: Duration,
        cancellation: Arc<Cancellation>,
    ) -> Result<Vec<Json>, String> {
        *self.cancellation.lock().expect("cancellation") = cancellation.clone();
        *self.deadline.lock().expect("deadline") = Instant::now() + timeout;
        *self.output.lock().expect("output") = Output::default();
        self.context.with(|ctx| {
            let result = ctx
                .eval_promise(code)
                .and_then(|promise| promise.finish::<Value>());
            match result {
                Ok(_) => Ok(()),
                Err(error) => {
                    let exception = ctx.catch();
                    let detail = ctx
                        .globals()
                        .get::<_, Function>("String")
                        .and_then(|function| function.call::<_, String>((exception,)))
                        .unwrap_or_else(|_| error.to_string());
                    if cancellation.is_cancelled() {
                        Err("Request cancelled. JavaScript state has been reset.".into())
                    } else if Instant::now() >= *self.deadline.lock().expect("deadline") {
                        Err(format!(
                            "{detail}. JavaScript state has been reset after timeout."
                        ))
                    } else {
                        Err(detail)
                    }
                }
            }
        })?;
        Ok(std::mem::take(
            &mut self.output.lock().expect("output").items,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        desktop::{Engine, accessibility::AtspiConnection},
        platform::drivers::SessionType,
        platform::registry::Registry,
    };
    #[tokio::test]
    async fn persistent_bindings_await_errors_reset_and_timeout() {
        let js = Javascript::new(Arc::new(Engine::new(
            Registry::default(),
            SessionType::X11,
            Arc::new(AtspiConnection::new()),
        )));
        js.evaluate(
            "const count = await Promise.resolve(7);".into(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        let result = js
            .evaluate("nodeRepl.write(count + 1);".into(), Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(result[0]["text"], "8");
        assert!(
            js.evaluate(
                "throw new Error('ordinary failure');".into(),
                Duration::from_secs(2)
            )
            .await
            .is_err()
        );
        let result = js
            .evaluate("nodeRepl.write(count);".into(), Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(result[0]["text"], "7");
        assert!(
            js.evaluate(
                "for(let i=0;i<65;i++) nodeRepl.write(i);".into(),
                Duration::from_secs(2)
            )
            .await
            .is_err()
        );
        assert!(
            js.evaluate("while (true) {}".into(), Duration::from_millis(30))
                .await
                .is_err()
        );
        let result = js
            .evaluate(
                "nodeRepl.write(typeof count);".into(),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(result[0]["text"], "undefined");
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                js.evaluate(
                    "const interrupted = 1; while(true) {}".into(),
                    Duration::from_secs(30)
                )
            )
            .await
            .is_err()
        );
        let result = js
            .evaluate(
                "nodeRepl.write(typeof interrupted);".into(),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(result[0]["text"], "undefined");
        js.evaluate("let restored = 9;".into(), Duration::from_secs(2))
            .await
            .unwrap();
        js.reset().await.unwrap();
        let result = js
            .evaluate(
                "nodeRepl.write(typeof restored);".into(),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(result[0]["text"], "undefined");
    }
}
