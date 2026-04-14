/// Processing state of a single thumbnail, mirroring the `thumbnails.status` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum ThumbnailStatus {
    Pending = 0,
    Ready = 1,
    Failed = 2,
}

impl ThumbnailStatus {
    pub fn from_i64(v: i64) -> Self {
        match v {
            1 => Self::Ready,
            2 => Self::Failed,
            _ => Self::Pending,
        }
    }
}
