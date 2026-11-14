use std::fmt;

/// Errors raised by the Python-free core.
#[derive(Debug)]
pub enum MeshError {
    /// A filesystem or stream error while reading/writing a mesh.
    Io(std::io::Error),
    /// The extension is not one of the supported mesh formats.
    UnsupportedExtension(String),
    /// A triangle index points outside the vertex array.
    IndexOutOfRange {
        triangle: usize,
        index: i64,
        vertices: usize,
    },
    /// The container/file is structurally invalid.
    Malformed(String),
}

impl fmt::Display for MeshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MeshError::Io(e) => write!(f, "{e}"),
            MeshError::UnsupportedExtension(ext) => {
                write!(f, "unsupported mesh extension: {ext}")
            }
            MeshError::IndexOutOfRange {
                triangle,
                index,
                vertices,
            } => write!(
                f,
                "triangle {triangle} references vertex {index} but the mesh has {vertices} vertices"
            ),
            MeshError::Malformed(msg) => write!(f, "malformed mesh: {msg}"),
        }
    }
}

impl std::error::Error for MeshError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MeshError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for MeshError {
    fn from(value: std::io::Error) -> Self {
        MeshError::Io(value)
    }
}

pub type MeshResult<T> = Result<T, MeshError>;
