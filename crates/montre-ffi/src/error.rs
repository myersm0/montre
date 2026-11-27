use std::any::Any;
use std::cell::{Cell, RefCell};
use std::ffi::CString;
use std::os::raw::c_char;
use std::panic::{self, AssertUnwindSafe};
use std::ptr;

use montre_query::QueryError;

/// The kind of the last error, from `montre_last_error_kind`. The values are
/// part of the C API and do not change.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
	None = 0,
	Other = 1,
	QueryParse = 2,
	QueryInvalid = 3,
	LimitExceeded = 4,
}

impl From<&QueryError> for ErrorKind {
	fn from(error: &QueryError) -> Self {
		match error {
			QueryError::Parse { .. } | QueryError::Regex(_) => ErrorKind::QueryParse,
			QueryError::UnknownLayer(_)
			| QueryError::UnindexedLayer(_)
			| QueryError::UnknownSpanLayer(_)
			| QueryError::UnknownComponent(_)
			| QueryError::UnknownDocument(_)
			| QueryError::UnknownLabel(_)
			| QueryError::QuantifierTooLarge { .. } => ErrorKind::QueryInvalid,
			QueryError::LimitExceeded { .. } => ErrorKind::LimitExceeded,
			QueryError::Execution(_) => ErrorKind::Other,
		}
	}
}

thread_local! {
	static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
	static LAST_ERROR_KIND: Cell<ErrorKind> = const { Cell::new(ErrorKind::None) };
}

fn set_error_of_kind(msg: String, kind: ErrorKind) {
	let c =
		CString::new(msg).unwrap_or_else(|_| CString::new("(error contained null byte)").unwrap());
	LAST_ERROR.with(|e| *e.borrow_mut() = Some(c));
	LAST_ERROR_KIND.with(|k| k.set(kind));
}

pub(crate) fn set_error(msg: String) {
	set_error_of_kind(msg, ErrorKind::Other);
}

pub(crate) fn set_query_error(error: QueryError) {
	set_error_of_kind(error.to_string(), ErrorKind::from(&error));
}

/// Runs `body`, turning a panic into an internal error and `None`, so that no
/// panic unwinds out of an `extern "C"` function and aborts the host.
pub(crate) fn contain_panics<T>(body: impl FnOnce() -> Option<T>) -> Option<T> {
	match panic::catch_unwind(AssertUnwindSafe(body)) {
		Ok(value) => value,
		Err(payload) => {
			set_error(format!("internal error: {}", panic_message(&*payload)));
			None
		}
	}
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
	payload
		.downcast_ref::<&str>()
		.copied()
		.or_else(|| payload.downcast_ref::<String>().map(String::as_str))
		.unwrap_or("panic with a non-string payload")
}

pub(crate) fn clear_error() {
	LAST_ERROR.with(|e| *e.borrow_mut() = None);
	LAST_ERROR_KIND.with(|k| k.set(ErrorKind::None));
}

#[no_mangle]
pub extern "C" fn montre_last_error() -> *const c_char {
	LAST_ERROR.with(|e| match *e.borrow() {
		Some(ref msg) => msg.as_ptr(),
		None => ptr::null(),
	})
}

/// The kind of the error `montre_last_error` describes, as an `ErrorKind`
/// value; 0 whenever `montre_last_error` returns null.
#[no_mangle]
pub extern "C" fn montre_last_error_kind() -> i32 {
	LAST_ERROR_KIND.with(|k| k.get() as i32)
}

#[cfg(test)]
mod tests {
	use std::ffi::CStr;

	use super::*;

	fn last_error() -> String {
		let message = montre_last_error();
		assert!(!message.is_null());
		unsafe { CStr::from_ptr(message) }
			.to_str()
			.unwrap()
			.to_string()
	}

	#[test]
	fn a_panic_is_contained_as_an_internal_error() {
		clear_error();
		assert_eq!(contain_panics(|| -> Option<u8> { panic!("boom") }), None);
		assert_eq!(montre_last_error_kind(), ErrorKind::Other as i32);
		assert_eq!(last_error(), "internal error: boom");

		let position = 7;
		assert_eq!(
			contain_panics(|| -> Option<u8> { panic!("no token at {position}") }),
			None
		);
		assert_eq!(last_error(), "internal error: no token at 7");
	}

	#[test]
	fn a_body_that_does_not_panic_passes_its_value_through() {
		clear_error();
		assert_eq!(contain_panics(|| Some(3)), Some(3));
		assert_eq!(montre_last_error_kind(), ErrorKind::None as i32);
	}
}
