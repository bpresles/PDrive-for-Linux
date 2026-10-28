//! Runs `pdrived` as a child process and restarts it when it fails, with a growing delay
//! (like `Restart=on-failure`). It is not started without a session in the keyring, nor when
//! another daemon already owns the D-Bus name.

use std::cell::RefCell;
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::gio;
use gtk::glib::{self, prelude::*};

use crate::daemon::BUS_NAME;

const FIRST_DELAY: Duration = Duration::from_secs(5);
const MAX_DELAY: Duration = Duration::from_secs(5 * 60);
/// A daemon that ran this long was healthy: the next failure restarts quickly again.
const HEALTHY_AFTER: Duration = Duration::from_secs(60);
/// Time given to the daemon to unmount and stop after SIGTERM, before SIGKILL.
const STOP_TIMEOUT: Duration = Duration::from_secs(20);

/// Path of a program installed next to this one, otherwise looked up in the PATH.
pub fn program(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(name)))
        .filter(|path| path.exists())
        .unwrap_or_else(|| name.into())
}

struct Inner {
    /// `None` until the keyring has been checked.
    signed_in: Option<bool>,
    pid: Option<u32>,
    started: Option<Instant>,
    /// No (re)start: the daemon is being stopped on purpose.
    paused: bool,
    checking: bool,
    retry: Option<glib::SourceId>,
    delay: Duration,
    waiters: Vec<Box<dyn FnOnce()>>,
}

pub struct Supervisor {
    inner: RefCell<Inner>,
    on_change: Box<dyn Fn()>,
}

impl Supervisor {
    pub fn new(on_change: impl Fn() + 'static) -> Rc<Self> {
        Rc::new(Self {
            inner: RefCell::new(Inner {
                signed_in: None,
                pid: None,
                started: None,
                paused: false,
                checking: false,
                retry: None,
                delay: FIRST_DELAY,
                waiters: Vec::new(),
            }),
            on_change: Box::new(on_change),
        })
    }

    pub fn signed_in(&self) -> Option<bool> {
        self.inner.borrow().signed_in
    }

    pub fn set_signed_in(&self, signed_in: bool) {
        self.inner.borrow_mut().signed_in = Some(signed_in);
        (self.on_change)();
    }

    /// The daemon runs, or is about to be started again.
    pub fn active(&self) -> bool {
        let inner = self.inner.borrow();
        inner.pid.is_some() || inner.retry.is_some() || inner.checking
    }

    /// Allows starting the daemon again after [`Supervisor::stop`].
    pub fn resume(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.paused = false;
        inner.delay = FIRST_DELAY;
    }

    /// Starts the daemon if it must run and does not.
    pub fn ensure_running(self: &Rc<Self>) {
        {
            let mut inner = self.inner.borrow_mut();
            if inner.paused || inner.pid.is_some() || inner.checking || inner.retry.is_some() {
                return;
            }
            inner.checking = true;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let signed_in = check_signed_in().await;
            let owned = if signed_in { name_has_owner().await } else { false };
            this.inner.borrow_mut().checking = false;
            this.inner.borrow_mut().signed_in = Some(signed_in);
            if signed_in && !owned && !this.inner.borrow().paused {
                this.spawn();
            }
            (this.on_change)();
        });
    }

    fn spawn(self: &Rc<Self>) {
        let mut command = Command::new(program("pdrived"));
        command.stdin(Stdio::null());
        // The daemon logs on its standard output and error: relay both to this one's error output.
        if let Ok(stderr) = std::io::stderr().as_fd().try_clone_to_owned() {
            command.stdout(Stdio::from(stderr));
        }
        // Runs in the child between fork and exec. PDEATHSIG is tied to the thread that forked:
        // this is called from the main loop, so on the main thread, which lives as long as the app.
        let parent = unsafe { libc::getpid() };
        unsafe {
            command.pre_exec(move || {
                // `main` blocks SIGTERM and SIGINT, and the mask is inherited through exec: the
                // daemon would never get the signals that stop it.
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
                // Stop with the app, even if it is killed: otherwise the daemon would be orphaned.
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                // The parent may have died before the call above.
                if libc::getppid() != parent {
                    return Err(std::io::Error::other("the application is gone"));
                }
                Ok(())
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                eprintln!("cannot start pdrived: {e}");
                return self.schedule_retry();
            }
        };
        eprintln!("pdrived started (pid {})", child.id());
        {
            let mut inner = self.inner.borrow_mut();
            inner.pid = Some(child.id());
            inner.started = Some(Instant::now());
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let status = gio::spawn_blocking(move || child.wait()).await;
            this.exited(status.ok().and_then(Result::ok));
        });
    }

    fn exited(self: &Rc<Self>, status: Option<std::process::ExitStatus>) {
        eprintln!("pdrived stopped ({})", status.map_or("unknown status".to_owned(), |s| s.to_string()));
        let (waiters, paused) = {
            let mut inner = self.inner.borrow_mut();
            inner.pid = None;
            if inner.started.is_some_and(|t| t.elapsed() > HEALTHY_AFTER) {
                inner.delay = FIRST_DELAY;
            }
            (std::mem::take(&mut inner.waiters), inner.paused)
        };
        for waiter in waiters {
            waiter();
        }
        if !paused && !status.is_some_and(|s| s.success()) {
            self.schedule_retry();
        }
        (self.on_change)();
    }

    fn schedule_retry(self: &Rc<Self>) {
        let mut inner = self.inner.borrow_mut();
        let delay = inner.delay;
        inner.delay = (delay * 2).min(MAX_DELAY);
        eprintln!("restarting pdrived in {} s", delay.as_secs());
        let this = self.clone();
        inner.retry = Some(glib::timeout_add_local_once(delay, move || {
            this.inner.borrow_mut().retry = None;
            this.ensure_running();
        }));
    }

    /// Stops the daemon (SIGTERM, then SIGKILL after a delay) and keeps it stopped until
    /// [`Supervisor::resume`]; `then` runs once it has ended (at once if it is not running).
    pub fn stop(self: &Rc<Self>, then: impl FnOnce() + 'static) {
        let running = {
            let mut inner = self.inner.borrow_mut();
            inner.paused = true;
            if let Some(retry) = inner.retry.take() {
                retry.remove();
            }
            inner.pid
        };
        let Some(pid) = running else { return then() };
        self.inner.borrow_mut().waiters.push(Box::new(then));
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        let this = self.clone();
        glib::timeout_add_local_once(STOP_TIMEOUT, move || {
            if this.inner.borrow().pid == Some(pid) {
                eprintln!("pdrived did not stop in time: killing it");
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            }
        });
    }
}

async fn check_signed_in() -> bool {
    let status = gio::spawn_blocking(|| Command::new(program("pdrive")).arg("status").stdout(Stdio::null()).status()).await;
    matches!(status, Ok(Ok(status)) if status.success())
}

async fn name_has_owner() -> bool {
    let Ok(connection) = gio::bus_get_future(gio::BusType::Session).await else { return false };
    let reply = connection
        .call_future(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(BUS_NAME,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
        )
        .await;
    reply.ok().and_then(|r| r.get::<(bool,)>()).is_some_and(|(owned,)| owned)
}
