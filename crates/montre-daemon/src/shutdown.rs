use std::io;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub(crate) struct ShutdownCoordinator {
	flag: AtomicBool,
	streams: Mutex<Vec<UnixStream>>,
	socket_path: PathBuf,
}

impl ShutdownCoordinator {
	pub(crate) fn new(socket_path: PathBuf) -> Self {
		Self {
			flag: AtomicBool::new(false),
			streams: Mutex::new(Vec::new()),
			socket_path,
		}
	}

	pub(crate) fn is_shutting_down(&self) -> bool {
		self.flag.load(Ordering::SeqCst)
	}

	pub(crate) fn mark_shutting_down(&self) -> bool {
		!self.flag.swap(true, Ordering::SeqCst)
	}

	pub(crate) fn register_stream(&self, stream: UnixStream) {
		let mut guard = self.streams.lock().expect("shutdown streams lock poisoned");
		guard.push(stream);
	}

	pub(crate) fn broadcast_frame(&self, payload: &[u8]) {
		let mut guard = self.streams.lock().expect("shutdown streams lock poisoned");
		for stream in guard.iter_mut() {
			let _ = crate::dispatch::write_frame(stream, payload);
		}
	}

	pub(crate) fn close_all_streams(&self) {
		let mut guard = self.streams.lock().expect("shutdown streams lock poisoned");
		for stream in guard.drain(..) {
			let _ = stream.shutdown(Shutdown::Both);
		}
	}

	pub(crate) fn wake_listener(&self) -> io::Result<()> {
		let stream = UnixStream::connect(&self.socket_path)?;
		drop(stream);
		Ok(())
	}

	pub(crate) fn socket_path(&self) -> &Path {
		&self.socket_path
	}
}

#[cfg(test)]
impl ShutdownCoordinator {
	pub(crate) fn dummy() -> std::sync::Arc<Self> {
		std::sync::Arc::new(Self::new(PathBuf::from("/tmp/montre-shutdown-test.sock")))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn mark_shutting_down_is_idempotent_and_first_caller_wins() {
		let coordinator = ShutdownCoordinator::new(PathBuf::from("/tmp/test.sock"));
		assert!(!coordinator.is_shutting_down());
		assert!(coordinator.mark_shutting_down());
		assert!(coordinator.is_shutting_down());
		assert!(!coordinator.mark_shutting_down());
		assert!(coordinator.is_shutting_down());
	}

	#[test]
	fn close_all_streams_drains_registered_streams() {
		let coordinator = ShutdownCoordinator::new(PathBuf::from("/tmp/test.sock"));
		assert_eq!(coordinator.streams.lock().unwrap().len(), 0);
		let (a, _b) = UnixStream::pair().expect("pair");
		coordinator.register_stream(a);
		assert_eq!(coordinator.streams.lock().unwrap().len(), 1);
		coordinator.close_all_streams();
		assert_eq!(coordinator.streams.lock().unwrap().len(), 0);
	}
}
