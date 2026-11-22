//! A print on a resin printer, on a thread of its own (the link's requests block on the
//! network): the exported stack goes over, the job starts, and the printer is asked how far it
//! is every so often until the job ends. Pause, resume and stop are asked of it in between, and
//! a stop before the job started cancels it: the printer prints nothing.
//!
//! A job Fab started before a restart is found again ([`Printing::follow`]): the printer is only
//! asked how far it is, and asked nothing else until an ask comes.
//!
//! The thread says what it sees in [`Seen`] and wakes its owner on every change. Dropping
//! [`Printing`] lets the job go: the thread says goodbye to the printer, which carries on by
//! itself.

use super::{ResinLink, State, Status};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the printer has to answer a request.
pub const WAIT: Duration = Duration::from_secs(10);

/// What is asked of the printer while its job runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    Pause,
    Resume,
    /// Before the job started: cancel it.
    Stop,
}

/// How far the print is.
#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    Connecting,
    /// `sent` of the stack's `of` layers are on the printer.
    Sending { sent: u32, of: u32 },
    /// The stack is over: the job is asked to start.
    Starting,
    /// The job runs: [`Seen::status`] says how far.
    Following,
    Ended(End),
}

/// How a print ended.
#[derive(Clone, Debug, PartialEq)]
pub enum End {
    /// Its last layer cured.
    Done,
    Stopped,
    /// Stopped before it started: nothing printed.
    Cancelled,
    /// A job found again isn't the printer's (it is idle, or its job has other layers): it ended
    /// while Fab was closed, how isn't known.
    Unseen,
    Failed(String),
}

/// What the thread saw last.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    pub stage: Stage,
    /// The printer's name, as it said hello.
    pub printer: String,
    /// The printer's last answer to STATUS.
    pub status: Option<Status>,
    /// Why the printer refused the last ask, if it did.
    pub refused: Option<String>,
}

/// A print on its thread.
pub struct Printing {
    seen: Arc<Mutex<Seen>>,
    asks: Sender<Ask>,
}

impl Printing {
    /// Prints the stack exported in `dir` on the printer at `addr`, asking how far it is every
    /// `every`. `wake` is called on every change of what is seen.
    pub fn start(addr: SocketAddr, dir: PathBuf, every: Duration, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Printing, String> {
        Printing::spawn(addr, Job::Send(dir), every, wake)
    }

    /// Follows the job of `layers` layers Fab gave the printer at `addr` before it restarted:
    /// the printer is asked how far it is every `every`, and nothing else until an ask. A
    /// printer whose job isn't that one ends it [`End::Unseen`].
    pub fn follow(addr: SocketAddr, layers: u32, every: Duration, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Printing, String> {
        Printing::spawn(addr, Job::Find(layers), every, wake)
    }

    fn spawn(addr: SocketAddr, job: Job, every: Duration, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Printing, String> {
        let seen = Arc::new(Mutex::new(Seen { stage: Stage::Connecting, printer: String::new(), status: None, refused: None }));
        let (asks, heard) = channel();
        let shared = seen.clone();
        std::thread::Builder::new().name("fab-resin-link".into()).spawn(move || run(addr, job, every, &shared, &heard, &*wake)).map_err(|e| format!("starting the printer's link: {e}"))?;
        Ok(Printing { seen, asks })
    }

    pub fn seen(&self) -> Seen {
        self.seen.lock().unwrap().clone()
    }

    /// Asks it of the printer (an ask after the job ended does nothing).
    pub fn ask(&self, ask: Ask) {
        let _ = self.asks.send(ask);
    }
}

/// What the thread does before it follows the job.
enum Job {
    /// Sends the stack exported in the folder and starts it.
    Send(PathBuf),
    /// Finds the printer's job of this many layers.
    Find(u32),
}

/// The print, start to end.
fn run(addr: SocketAddr, job: Job, every: Duration, seen: &Mutex<Seen>, asks: &Receiver<Ask>, wake: &(dyn Fn() + Send + Sync)) {
    let set = |f: &dyn Fn(&mut Seen)| {
        f(&mut seen.lock().unwrap());
        wake();
    };
    let end = |e: End| set(&|s| s.stage = Stage::Ended(e.clone()));
    let mut link = match ResinLink::connect(addr, WAIT) {
        Ok(link) => link,
        Err(e) => return end(End::Failed(e)),
    };
    let name = link.printer().to_string();
    set(&|s| s.printer = name.clone());
    let mut find = match job {
        Job::Send(dir) => {
            // a stop before the job starts, heard between the files, cancels it
            let mut cancelled = false;
            let mut stopped = || {
                cancelled |= asks.try_iter().any(|a| a == Ask::Stop);
                !cancelled
            };
            if !stopped() {
                return end(End::Cancelled);
            }
            let sent = link.send_stack(&dir, &mut |sent, of| {
                set(&|s| s.stage = Stage::Sending { sent, of });
                stopped()
            });
            if cancelled {
                return end(End::Cancelled);
            }
            if let Err(e) = sent {
                return end(End::Failed(e));
            }
            set(&|s| s.stage = Stage::Starting);
            if let Err(e) = link.print() {
                return end(End::Failed(e));
            }
            set(&|s| s.stage = Stage::Following);
            None
        }
        Job::Find(layers) => Some(layers),
    };
    loop {
        let status = match link.status() {
            Ok(status) => status,
            Err(e) => return end(End::Failed(e)),
        };
        // the job found again is the printer's only while the printer has a job of its layers
        if find.take().is_some_and(|layers| status.state == State::Idle || status.layers != layers) {
            return end(End::Unseen);
        }
        let stage = match status.state {
            State::Done => Stage::Ended(End::Done),
            State::Stopped => Stage::Ended(End::Stopped),
            State::Idle => Stage::Ended(End::Failed("the printer let the job go".into())),
            State::Printing | State::Paused => Stage::Following,
        };
        let ended = matches!(stage, Stage::Ended(_));
        set(&|s| {
            s.status = Some(status);
            s.stage = stage.clone();
        });
        if ended {
            return;
        }
        match asks.recv_timeout(every) {
            Ok(ask) => {
                let answer = match ask {
                    Ask::Pause => link.pause(),
                    Ask::Resume => link.resume(),
                    Ask::Stop => link.stop(),
                };
                set(&|s| s.refused = answer.clone().err());
            }
            Err(RecvTimeoutError::Timeout) => {}
            // the owner let the job go: the link says goodbye as it drops
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
