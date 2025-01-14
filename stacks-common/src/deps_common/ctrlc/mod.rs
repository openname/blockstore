// Copyright (c) 2017 CtrlC developers
// Licensed under the Apache License, Version 2.0
// <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT
// license <LICENSE-MIT or http://opensource.org/licenses/MIT>,
// at your option. All files in the project carrying such
// notice may not be copied, modified, or distributed except
// according to those terms.

#![macro_use]

mod error;
mod platform;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

pub use self::error::Error;

#[cfg(test)]
mod tests;

#[derive(PartialEq, Clone)]
#[repr(u8)]
pub enum SignalId {
    CtrlC = 0x00,
    Termination = 0x01,
    Bus = 0x02,
    Other = 0xff,
}

impl std::fmt::Display for SignalId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        match *self {
            SignalId::CtrlC => write!(f, "CtrlC"),
            SignalId::Termination => write!(f, "Termination"),
            SignalId::Bus => write!(f, "Bus"),
            SignalId::Other => write!(f, "Other"),
        }
    }
}

static INIT: AtomicBool = AtomicBool::new(false);

/// Register signal handler for Ctrl-C.
///
/// Starts a new dedicated signal handling thread. Should only be called once,
/// typically at the start of your program.
///
/// # Warning
/// On Unix, any existing `SIGINT`, `SIGTERM`, `SIGHUP`, `SIGBUS`, or `SA_SIGINFO`
/// posix signal handlers will be overwritten. On Windows, multiple handler routines are allowed,
/// but they are called on a last-registered, first-called basis until the signal is handled.
///
/// On Unix, signal dispositions and signal handlers are inherited by child processes created via
/// `fork(2)` on, but not by child processes created via `execve(2)`.
/// Signal handlers are not inherited on Windows.
///
/// # Errors
/// Will return an error if another `ctrlc::set_handler()` handler exists or if a
/// system error occurred while setting the handler.
///
/// # Panics
/// Any panic in the handler will not be caught and will cause the signal handler thread to stop.
///
pub fn set_handler<F>(mut user_handler: F) -> Result<(), Error>
where
    F: FnMut(SignalId) + 'static + Send,
{
    if INIT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(Error::MultipleHandlers);
    }

    unsafe {
        match platform::init_os_handler() {
            Ok(_) => {}
            Err(err) => {
                INIT.store(false, Ordering::SeqCst);
                return Err(err.into());
            }
        }
    }

    thread::Builder::new()
        .name("signal-handler".into())
        .spawn(move || loop {
            let received_signal = unsafe {
                platform::block_ctrl_c()
                    .expect("Critical system error while waiting for terminating signal")
            };
            user_handler(received_signal);
        })
        .expect("failed to spawn thread");

    Ok(())
}
