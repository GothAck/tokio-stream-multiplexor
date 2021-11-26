#[derive(Copy, Clone, Debug)]
pub struct StreamMultiplexorConfig {
    pub max_frame_size: usize,
    pub buf_size: usize,
    pub max_queued_frames: usize,
    pub accept_queue_len: usize,
    pub identifier: &'static str,
}

impl Default for StreamMultiplexorConfig {
    /// Construct a default StreamMultiplexorConfig
    fn default() -> Self {
        Self {
            max_frame_size: 4 * 1024 * 1024,
            buf_size: 1024 * 1024,
            max_queued_frames: 256,
            accept_queue_len: 16,
            identifier: "",
        }
    }
}

impl StreamMultiplexorConfig {
    /// Add identifier static &str to config
    pub fn with_identifier(mut self, identifier: &'static str) -> Self {
        self.identifier = identifier;
        self
    }
}
