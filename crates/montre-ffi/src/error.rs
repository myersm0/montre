use std::cell::{Cell, RefCell};
use std::ffi::CString;
use std::os::raw::c_char;
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
