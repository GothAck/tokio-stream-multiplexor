#[derive(Copy, Clone, Debug)]
pub struct StreamMultiplexorConfig {
    pub max_frame_size: usize,
    pub buf_size: usize,
    pub identifier: &'static str,
}

impl Default for StreamMultiplexorConfig {
    /// Construct a default StreamMultiplexorConfig
    fn default() -> Self {
        Self {
            max_frame_size: 4 * 1024 * 1024,
            buf_size: 1024 * 1024,
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
