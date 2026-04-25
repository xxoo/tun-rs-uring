use std::io;

#[allow(dead_code)]
pub(crate) fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[allow(dead_code)]
pub(crate) fn invalid_config(field: &str, message: impl Into<String>) -> io::Error {
    invalid_input(format!("invalid `config.{field}`: {}", message.into()))
}

#[allow(dead_code)]
pub(crate) fn unsupported(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message.into())
}

#[allow(dead_code)]
pub(crate) fn no_async_backend() -> io::Error {
    unsupported("no async backend enabled; enable feature `async_tokio` or `async_io`")
}

#[allow(dead_code)]
pub(crate) fn rx_fast_poll_required() -> io::Error {
    unsupported("rx io_uring requires IORING_FEAT_FAST_POLL")
}

#[allow(dead_code)]
pub(crate) fn rx_stopped() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "rx stopped")
}

#[allow(dead_code)]
pub(crate) fn rx_driver_closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "rx driver closed")
}

#[allow(dead_code)]
pub(crate) fn rx_buffers_exhausted() -> io::Error {
    io::Error::from_raw_os_error(libc::ENOBUFS)
}

#[allow(dead_code)]
pub(crate) fn clone_io_error(error: &io::Error) -> io::Error {
    match error.raw_os_error() {
        Some(code) => io::Error::from_raw_os_error(code),
        None => io::Error::new(error.kind(), error.to_string()),
    }
}
