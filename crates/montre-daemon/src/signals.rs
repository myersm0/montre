use std::io;
use std::sync::mpsc::{sync_channel, Sender};
use std::thread::{self, JoinHandle};

use signal_hook::consts::{SIGHUP, SIGINT, SIGPIPE, SIGTERM};
use signal_hook::iterator::{Handle, Signals};

use crate::protocol::ShutdownReason;
use crate::state::Command;

pub(crate) fn install_signal_thread(
	state_tx: Sender<Command>,
) -> io::Result<(Handle, JoinHandle<()>)> {
	let mut signals = Signals::new([SIGHUP, SIGINT, SIGPIPE, SIGTERM])?;
	let handle = signals.handle();

	let thread = thread::spawn(move || {
		for signal in signals.forever() {
			match signal {
				SIGPIPE => {
					tracing::trace!("SIGPIPE received; ignored");
					continue;
				}
				SIGHUP | SIGINT | SIGTERM => {
					let signal_name = match signal {
						SIGHUP => "SIGHUP",
						SIGINT => "SIGINT",
						SIGTERM => "SIGTERM",
						_ => unreachable!(),
					};
					tracing::info!(signal = signal_name, "shutdown signal received");
					let (reply_tx, reply_rx) = sync_channel(1);
					if state_tx
						.send(Command::InitiateShutdown {
							reason: ShutdownReason::Signal,
							reply: reply_tx,
						})
						.is_ok()
					{
						let _ = reply_rx.recv();
					}
					break;
				}
				other => {
					tracing::warn!(signal = other, "unexpected signal received");
				}
			}
		}
	});

	Ok((handle, thread))
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::mpsc::channel;

	#[test]
	fn install_and_close_exits_cleanly() {
		let (state_tx, _state_rx) = channel();
		let (handle, thread) = install_signal_thread(state_tx).expect("install");
		handle.close();
		thread.join().expect("join");
	}
}
