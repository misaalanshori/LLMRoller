use super::*;

/// Holds one in-flight slot on the selected route until the attempt or stream finishes.
pub(crate) struct InFlightGuard {
    pub(crate) tracker: Arc<llmroller_core::runtime_state::LoadTracker>,
    pub(crate) route: RouteId,
}

impl InFlightGuard {
    pub(crate) fn new(tracker: Arc<llmroller_core::runtime_state::LoadTracker>, route: RouteId) -> Self {
        tracker.begin(route);
        Self { tracker, route }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.tracker.end(self.route);
    }
}

pub(crate) fn upstream_content_encoding(headers: &HeaderMap) -> Option<String> {
    headers
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("identity"))
        .map(ToOwned::to_owned)
}

pub(crate) fn decode_upstream_body(encoding: Option<&str>, body: Bytes) -> Result<Bytes, ForwardError> {
    match encoding {
        Some(encoding) => decompress_chunk(encoding, &body).map(Bytes::from),
        None => Ok(body),
    }
}

type UpstreamByteStream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

/// Decode a possibly compressed upstream body as a stateful stream so chunk boundaries
/// never matter (TRN-3). Unsupported encodings fail the stream and trigger failover.
pub(crate) fn cap_decompressed_stream<S>(inner: S) -> UpstreamByteStream
where
    S: futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
{
    let mut cumulative = 0usize;
    Box::pin(inner.map(move |res| match res {
        Ok(chunk) => {
            cumulative = cumulative.saturating_add(chunk.len());
            if cumulative > crate::forward::MAX_DECOMPRESSED_BYTES {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "SEC-8 decompressed stream exceeded 16 MiB limit",
                ))
            } else {
                Ok(chunk)
            }
        }
        Err(e) => Err(e),
    }))
}

pub(crate) fn decode_upstream_stream(
    response: reqwest::Response,
    encoding: Option<String>,
) -> UpstreamByteStream {
    let raw = response
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    let Some(encoding) = encoding else {
        return Box::pin(raw);
    };
    let normalized = encoding.to_ascii_lowercase();
    let reader = StreamReader::new(raw);
    match normalized.as_str() {
        "gzip" => cap_decompressed_stream(ReaderStream::new(GzipDecoder::new(reader))),
        "deflate" => cap_decompressed_stream(ReaderStream::new(DeflateDecoder::new(reader))),
        "zlib" => cap_decompressed_stream(ReaderStream::new(ZlibDecoder::new(reader))),
        "br" => cap_decompressed_stream(ReaderStream::new(BrotliDecoder::new(reader))),
        "zstd" => cap_decompressed_stream(ReaderStream::new(ZstdDecoder::new(reader))),
        _ => {
            let message = format!("unsupported content-encoding: {encoding}");
            Box::pin(stream::once(async move {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    message,
                ))
            }))
        }
    }
}

pub(crate) struct StreamState {
    pub(crate) upstream: UpstreamByteStream,
    pub(crate) pending: VecDeque<Bytes>,
    pub(crate) forwarder: SseChunkForwarder,
    pub(crate) transcoder: Option<llmroller_proto::transcode::SseStreamTranscoder>,
    pub(crate) finalizer: Option<StreamFinalizer>,
    pub(crate) lifecycle: RequestLifecycle,
    /// Gap allowed between upstream bytes after the first valid frame.
    pub(crate) idle_timeout: Duration,
    /// Set by the shutdown supervisor before it wakes parked streams.
    pub(crate) shutdown_cut: Arc<AtomicBool>,
    pub(crate) shutdown_notify: Option<Arc<tokio::sync::Notify>>,
    /// Held for exactly the lifetime of the response body stream.
    pub(crate) _stream_guard: ActiveStreamGuard,
    /// Keeps P2C in-flight load accurate until the response body ends.
    pub(crate) _in_flight: InFlightGuard,
    pub(crate) done: bool,
}

impl StreamState {
    /// Finalize accounting and the lifecycle guard exactly once for a terminal step.
    pub(crate) fn finish(&mut self, termination: Termination) {
        if let Some(finalizer) = &mut self.finalizer {
            finalizer.finalize(termination);
        }
        self.lifecycle.set_termination(termination);
        self.lifecycle.finalize();
        self.done = true;
    }
}

/// Outcome of one post-first-frame poll of the upstream body.
pub(crate) enum StreamStep {
    Item(Option<Result<Bytes, std::io::Error>>),
    Idle,
    Shutdown,
}

