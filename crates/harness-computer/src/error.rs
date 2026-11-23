use thiserror::Error;

#[derive(Error, Debug)]
pub enum ComputerError {
    #[error("Browser error: {0}")]
    Browser(String),

    #[error("Screen capture error: {0}")]
    ScreenCapture(String),

    #[error("Desktop input simulation error: {0}")]
    InputSimulation(String),

    #[error("Coordinates ({x}, {y}) out of screen bounds (width: {max_x}, height: {max_y})")]
    CoordinatesOutOfBounds {
        x: i32,
        y: i32,
        max_x: u32,
        max_y: u32,
    },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
