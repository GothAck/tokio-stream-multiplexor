#[derive(Copy, Clone, Debug)]
pub struct Config {
    pub max_frame_size: usize,
    pub buf_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_frame_size: 4 * 1024 * 1024,
            buf_size: 1024 * 1024,
        }
    }
}
