use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use pin_project_lite::pin_project;
use thiserror::Error;
use tokio::io::AsyncWrite;
use xz2::stream::{Status, Stream};

#[derive(Error, Debug)]
pub enum XZDecoderError {
    #[error("Got status {0:#?} during decompression!")]
    DecompressionError(Status),
    #[error("Error from xz2")]
    XZ2Error {
        #[from]
        source: xz2::stream::Error,
    },
    #[error("Got an IO error somehwere in the stack")]
    IO {
        #[from]
        source: io::Error,
    },
}

pin_project! {
    pub struct XZDecoder<W: AsyncWrite> {
        #[pin]
        inner_writer: W,
        // This is a buffer used only to communicate with the xz2 stuff. It doesn't mean that this XZDecoder acts like a BufWriter, although there is some amount of buffering going on in the current implementation, so calling `flush()` is still required to ensure everything is written into the inner writer.
        buffer: Box<[u8]>,
        // This is how much of the buffer we used so far.
        buffer_len: usize,
        // This is how much of the buffer we have written so far. Only matters when `buffer_len` > 0.
        written_len: usize,
        dec_stream: Stream,
    }
}

impl<W: AsyncWrite> XZDecoder<W> {
    pub fn new(inner_writer: W) -> Result<Self, XZDecoderError> {
        Ok(Self {
            inner_writer,
            dec_stream: Stream::new_stream_decoder(u64::MAX, 0)?,
            buffer: vec![0u8; 1 << 17].into_boxed_slice(),
            buffer_len: 0,
            written_len: 0,
        })
    }

    fn flush_buffer(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // println!(
        //     "Got called to flush the buffer, buffer len is {}",
        //     self.buffer_len
        // );

        if self.buffer_len > 0 {
            let this = self.project();
            // Means we still need to offload the results from the buffer first into the inner writer, so we'll do that.
            match this
                .inner_writer
                .poll_write(cx, &this.buffer[*this.written_len..*this.buffer_len])
            {
                // We'll let the inner writer control the waker.
                Poll::Pending => {
                    // println!("  Inner writer is pending");
                    Poll::Pending
                }
                Poll::Ready(Ok(n)) => {
                    // println!("  Inner writer wrote {} bytes", n);
                    // TODO: check if `n` == 0, and assume we won't ever be able to keep going if that's the case, and fail with an error accordingly.

                    *this.written_len += n;

                    if this.written_len > this.buffer_len {
                        unreachable!("broken assumption");
                    }

                    if this.written_len < this.buffer_len {
                        // We still have more to write to the inner writer, so we'll immediately signal the waker and wait for it to call us again.
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    } else {
                        // We wrote everything needed to the inner writer.
                        *this.written_len = 0;
                        *this.buffer_len = 0;
                        Poll::Ready(Ok(()))
                    }
                }
                Poll::Ready(Err(err)) => {
                    // println!("  Inner writer gave us an error!");
                    Poll::Ready(Err(err))
                }
            }
        } else {
            // Nothing to flush.
            Poll::Ready(Ok(()))
        }
    }
}

impl<W: AsyncWrite> AsyncWrite for XZDecoder<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        // println!(
        //     "Got called to poll_write with a buf of {} bytes!",
        //     buf.len()
        // );
        match self.as_mut().flush_buffer(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Ok(_)) => (),
            Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
        }
        // Assumption: if we're here, there's no data in `self.buffer` so we can use it completely.
        if self.buffer_len != 0 {
            unreachable!("broken assumption");
        }

        // Decompression process roughly inspired by https://github.com/near/nearcore/blob/6f607a2518f1e0b7377b42b0d9a94155cd9e0dcd/nearcore/src/download_file.rs#L226
        let mut this = self.project();
        let total_in = this.dec_stream.total_in();
        let total_out = this.dec_stream.total_out();
        // TODO: this is blocking code running in an async environment. It is expected to run quickly enough that spawning a new thread just to get this to run isn't worth it, but it's possible that if buffer sizes are large, spawning a new thread might be desirable. Figure out what to do.
        let process_result =
            this.dec_stream
                .process(buf, &mut this.buffer, xz2::stream::Action::Run);

        match process_result {
            Err(err) => {
                // TODO: improve error types.
                // println!("    xz2 stream gave us an error");
                return Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, err)));
            }
            Ok(xz2::stream::Status::Ok | xz2::stream::Status::StreamEnd) => (),
            Ok(status) => {
                // println!("    xz2 stream gave us an unexpected status");
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    XZDecoderError::DecompressionError(status),
                )));
            }
        }

        let read = (this.dec_stream.total_in() - total_in) as usize;
        let wrote = (this.dec_stream.total_out() - total_out) as usize;
        *this.buffer_len = wrote;
        // println!(
        //     "    xz2 stream read {} bytes from the poll_write, returning that we read this much.",
        //     read
        // );

        // We won't try to be fancy and make a call to the inner writer here, we'll just return that we're ready and we processed some input, and let further calls take care of emptying our output into the inner writer.
        Poll::Ready(Ok(read))
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        // println!("Got called to flush!");
        match self.as_mut().flush_buffer(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Ok(_)) => (),
            Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
        }
        // Assumption: if we're here, there's no data in `self.buffer` to flush anymore, so we'll just flush the inner writer.
        if self.buffer_len != 0 {
            unreachable!("broken assumption");
        }

        // println!("    We finished flushing our own buffer, so delegating to the inner writer now.");

        let this = self.project();
        this.inner_writer.poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        // println!("Got called to shutdown!");
        match self.as_mut().flush_buffer(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Ok(_)) => (),
            Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
        }
        // Assumption: if we're here, there's no data in `self.buffer` to flush anymore, so we'll just delegate to the inner writer.
        if self.buffer_len != 0 {
            unreachable!("broken assumption");
        }

        // println!("    We finished flushing our own buffer, so delegating to the inner writer now.");

        let this = self.project();
        this.inner_writer.poll_shutdown(cx)
    }
}
