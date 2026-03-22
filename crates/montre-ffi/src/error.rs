use std::cell::RefCell;
use std::ffi::CString;
use std::os::raw::c_char;
use std::ptr;

thread_local! {
	static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

pub(crate) fn set_error(msg: String) {
	let c = CString::new(msg).unwrap_or_else(|_| CString::new("(error contained null byte)").unwrap());
	LAST_ERROR.with(|e| *e.borrow_mut() = Some(c));
}

pub(crate) fn clear_error() {
	LAST_ERROR.with(|e| *e.borrow_mut() = None);
}

#[no_mangle]
pub extern "C" fn montre_last_error() -> *const c_char {
	LAST_ERROR.with(|e| {
		match *e.borrow() {
			Some(ref msg) => msg.as_ptr(),
			None => ptr::null(),
		}
	})
}
