
pub struct PError {
    code: i32,
    message: String,
}

impl PError {
    fn new(msg: &str) -> Self {
        Self {
            code: 0,
            message: msg.to_string(),
        }
    }
}

impl fmt::Display for PError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl fmt::Debug for PError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{{ file: {}, line: {} }}", file!(), line!()) // programmer-facing output
    }
}

impl error::Error for LexerError {
    fn description(&self) -> &str {
        &self.message
    }

    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        // The lower-level source of this error, if any.
        None
    }
}
