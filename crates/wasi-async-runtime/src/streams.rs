use core::{pin::Pin, task};

use crate::{reactor::PollHandle, Reactor};
use bytes::{Buf, BytesMut};
use futures_lite::{io, AsyncBufRead, AsyncRead, AsyncWrite};
use wasi::io::streams::{
    InputStream as WasiInputStream, OutputStream as WasiOutputStream,
    StreamError as WasiStreamError,
};

const DEFAULT_BUF_LEN: usize = 32;

#[derive(Debug)]
/// Wraps [wasi::io::streams::InputStream] to enable usage with async libraries.
pub struct InputStream {
    inner: WasiInputStream,
    poll_handle: Option<PollHandle>,
    buf: BytesMut,
}

impl InputStream {
    /// Instatiate the stream.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// block_on(|r| {
    ///     let lines = InputStream::new(my_wasi_input_stream, r.clone()).lines();
    ///     while let Some(line) = lines.next().await.transpose()? {
    ///         eprintln!("Got a line: {line}");
    ///     }
    /// });
    /// ```
    pub fn new(inner: WasiInputStream, reactor: Reactor) -> Self {
        let poll_handle = Some(reactor.register(inner.subscribe()));
        Self {
            inner,
            poll_handle,
            buf: Default::default(),
        }
    }

    fn poll(&self, cx: &mut task::Context<'_>) -> task::Poll<()> {
        self.poll_handle.as_ref().unwrap().poll(cx)
    }
}

impl Drop for InputStream {
    fn drop(&mut self) {
        // NOTE: we need to drop [PollHandle] first given the resource hierarchy
        self.poll_handle.take().unwrap();
    }
}

impl AsyncRead for InputStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &mut [u8],
    ) -> task::Poll<std::io::Result<usize>> {
        if self.poll(cx).is_pending() {
            return task::Poll::Pending;
        }

        let mut len = self.buf.remaining();
        let bytes = match len {
            0 => match self.inner.read(buf.len() as u64) {
                Ok(bytes) => bytes,
                Err(WasiStreamError::Closed) => return task::Poll::Ready(Ok(0)),
                Err(WasiStreamError::LastOperationFailed(err)) => {
                    return task::Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        err.to_debug_string(),
                    )))
                }
            },
            _ => {
                if buf.len() < len {
                    len = buf.len();
                }
                self.get_mut().buf.copy_to_bytes(len).to_vec()
            }
        };
        let len = bytes.len();
        bytes.into_iter().enumerate().for_each(|(i, b)| buf[i] = b);
        task::Poll::Ready(Ok(len))
    }
}

impl AsyncBufRead for InputStream {
    fn poll_fill_buf(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> task::Poll<std::io::Result<&[u8]>> {
        if self.poll(cx).is_pending() {
            return task::Poll::Pending;
        }

        let bytes = match self.inner.read(DEFAULT_BUF_LEN as u64) {
            Ok(bytes) => bytes,
            Err(WasiStreamError::Closed) => {
                return task::Poll::Ready(Ok(self.get_mut().buf.chunk()));
            }
            Err(WasiStreamError::LastOperationFailed(err)) => {
                return task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    err.to_debug_string(),
                )))
            }
        };
        let this = self.get_mut();
        this.buf.extend(bytes);
        task::Poll::Ready(Ok(this.buf.chunk()))
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        self.get_mut().buf.advance(amt);
    }
}

#[derive(Debug)]
/// Wraps [wasi::io::streams::OutputStream] to enable usage with async libraries.
pub struct OutputStream {
    inner: WasiOutputStream,
    poll_handle: Option<PollHandle>,
}

impl OutputStream {
    /// Instatiate the stream.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// block_on(|r| {
    ///     let output = OutputStream::new(my_wasi_output_stream, r.clone());
    ///     output.write_all("Hello world!\n".as_bytes()).await?;
    ///     output.flush().await?;
    /// });
    /// ```
    pub fn new(inner: WasiOutputStream, reactor: Reactor) -> Self {
        let poll_handle = Some(reactor.register(inner.subscribe()));
        Self { inner, poll_handle }
    }

    fn poll(&self, cx: &mut task::Context<'_>) -> task::Poll<()> {
        self.poll_handle.as_ref().unwrap().poll(cx)
    }
}

impl Drop for OutputStream {
    fn drop(&mut self) {
        // NOTE: we need to drop [PollHandle] first given the resource hierarchy
        self.poll_handle.take().unwrap();
    }
}

impl AsyncWrite for OutputStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &[u8],
    ) -> task::Poll<io::Result<usize>> {
        if self.poll(cx).is_pending() {
            return task::Poll::Pending;
        }

        let mut n = match self.inner.check_write() {
            Ok(n) => n as usize,
            Err(WasiStreamError::Closed) => {
                return task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "stream closed",
                )));
            }
            Err(WasiStreamError::LastOperationFailed(err)) => {
                return task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    err.to_debug_string(),
                )))
            }
        };
        if buf.len() < n {
            n = buf.len();
        }

        match self.inner.write(&buf[..n]) {
            Ok(()) => {}
            Err(WasiStreamError::Closed) => {
                return task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "stream closed",
                )));
            }
            Err(WasiStreamError::LastOperationFailed(err)) => {
                return task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    err.to_debug_string(),
                )))
            }
        }

        task::Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<io::Result<()>> {
        if self.poll(cx).is_pending() {
            return task::Poll::Pending;
        }

        match self.inner.flush() {
            Ok(()) => task::Poll::Ready(Ok(())),
            Err(err) => task::Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                match err {
                    WasiStreamError::Closed => "stream closed".to_owned(),
                    WasiStreamError::LastOperationFailed(err) => err.to_debug_string(),
                },
            ))),
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<io::Result<()>> {
        if self.poll(cx).is_pending() {
            return task::Poll::Pending;
        }

        // there's no underlying close fn, so flush instead and hope for the best
        self.poll_flush(cx)
    }
}
